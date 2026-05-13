//! Streaming entry point for `WorkerPool`: consume `WorkItem`s from an
//! `mpsc::Receiver`, enqueue them into the SQLite-backed `JobQueue`, and
//! claim/process them concurrently while an `execution_gate` is held open.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::progress::ProgressReporter;
use crate::worker::{
    JobErrorStrategy, JobResult, WorkItem, WorkerContext, WorkerPool, cancel_unstarted_jobs,
    run_claimed_job,
};

/// Polling interval workers use when the queue is momentarily empty but the
/// enqueuer has not yet signalled it is done. Kept small to keep latency low;
/// the loop only sleeps when there is genuinely no pending work.
const CLAIM_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Arguments shared across all workers in a single `process_stream` invocation.
///
/// Bundled into a struct to keep `spawn_worker`'s signature within the
/// `clippy::too_many_arguments` budget.
struct WorkerSpawnArgs<F> {
    producer_done_flag: Arc<AtomicBool>,
    execution_gate: Arc<Notify>,
    processor: Arc<F>,
    on_error: JobErrorStrategy,
    reporter: Arc<dyn ProgressReporter>,
    result_tx: mpsc::Sender<JobResult>,
}

impl WorkerPool {
    /// Stream entries through the pool: consume items from `items`, enqueue
    /// each one into the job queue, and run up to `effective_workers` claim
    /// loops in parallel. Workers wait on `execution_gate` before their first
    /// claim. The pool returns when:
    ///
    /// 1. the receiver has been drained (producer dropped the `tx` end),
    /// 2. and every worker has finished its last in-flight job.
    ///
    /// Workers claim work via [`crate::queue::JobQueue::claim`], which honors
    /// the SQLite store's priority ordering (`ORDER BY priority ASC, created_at
    /// ASC`). This is the same dispatch order used by `voom process` in batch
    /// mode and by `--priority-by-date`.
    ///
    /// Cancellation: the pool's internal token (passed via [`WorkerPool::new`])
    /// is the shared signal. When cancelled, the enqueuer stops draining the
    /// receiver, and workers exit after their current job. Any jobs that were
    /// enqueued but never claimed (still in `Pending` state) are cancelled via
    /// [`cancel_unstarted_jobs`].
    #[tracing::instrument(skip(self, items, execution_gate, processor, reporter))]
    pub async fn process_stream<P, F, Fut>(
        &self,
        items: mpsc::Receiver<WorkItem<P>>,
        execution_gate: Arc<Notify>,
        processor: F,
        on_error: JobErrorStrategy,
        reporter: Arc<dyn ProgressReporter>,
    ) -> Vec<JobResult>
    where
        P: serde::Serialize + Send + 'static,
        F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
            + Send
            + 'static,
    {
        let effective_workers = self.config.effective_workers();
        let processor = Arc::new(processor);

        tracing::info!(
            workers = effective_workers,
            "Starting streaming worker pool"
        );

        // Internal "producer done" flag. Flipped by the enqueuer when the
        // receiver returns None (tx was dropped) or when cancellation fires.
        // Using AtomicBool rather than Notify avoids the single-permit
        // consumption pitfall of Notify::notified() in a polling loop.
        let producer_done_flag = Arc::new(AtomicBool::new(false));

        // Track the ids we successfully enqueue in this run. On cancellation
        // cleanup we intersect this set with the queue's current Pending list
        // so we only cancel jobs this run produced — never unrelated Pending
        // rows that may already exist in a long-lived SQLite store.
        let enqueued_ids: Arc<Mutex<HashSet<Uuid>>> = Arc::new(Mutex::new(HashSet::new()));

        let (result_tx, mut result_rx) = mpsc::channel::<JobResult>(effective_workers.max(1) * 4);

        // --- Enqueuer task --------------------------------------------------
        let enqueuer_handle = self.spawn_enqueuer(
            items,
            producer_done_flag.clone(),
            enqueued_ids.clone(),
            reporter.clone(),
            result_tx.clone(),
        );

        // --- Worker tasks ---------------------------------------------------
        let mut worker_handles: Vec<JoinHandle<()>> = Vec::with_capacity(effective_workers);
        for _ in 0..effective_workers {
            let handle = self.spawn_worker(WorkerSpawnArgs {
                producer_done_flag: producer_done_flag.clone(),
                execution_gate: execution_gate.clone(),
                processor: processor.clone(),
                on_error,
                reporter: reporter.clone(),
                result_tx: result_tx.clone(),
            });
            worker_handles.push(handle);
        }

        // Drop the local sender so result_rx closes once all workers + the
        // enqueuer have dropped their clones.
        drop(result_tx);

        if let Err(e) = enqueuer_handle.await {
            tracing::error!(error = %e, "enqueuer task join error");
        }

        // Collect results as workers report them.
        let mut results = Vec::new();
        while let Some(result) = result_rx.recv().await {
            results.push(result);
        }

        for handle in worker_handles {
            if let Err(e) = handle.await {
                tracing::error!(error = %e, "worker task join error");
            }
        }

        // If we were cancelled, sweep any jobs that were enqueued but never
        // claimed. We restrict cancellation to ids we enqueued in this run —
        // the SQLite store is long-lived and may legitimately hold Pending
        // rows from prior crashes or other code paths.
        if self.token.is_cancelled() {
            let leftover = collect_unstarted_enqueued_ids(&self.queue, &enqueued_ids);
            cancel_unstarted_jobs(self.queue.clone(), leftover).await;
        }

        reporter.on_batch_complete(
            self.completed_count.load(Ordering::SeqCst),
            self.failed_count.load(Ordering::SeqCst),
        );

        results
    }

    /// Spawn the single enqueuer task that drains `items` into the job queue
    /// and flips `producer_done_flag` on completion. Every successful enqueue
    /// id is recorded in `enqueued_ids` so cancellation cleanup can scope its
    /// sweep to jobs produced by this run.
    fn spawn_enqueuer<P>(
        &self,
        mut items: mpsc::Receiver<WorkItem<P>>,
        producer_done_flag: Arc<AtomicBool>,
        enqueued_ids: Arc<Mutex<HashSet<Uuid>>>,
        reporter: Arc<dyn ProgressReporter>,
        result_tx: mpsc::Sender<JobResult>,
    ) -> JoinHandle<()>
    where
        P: serde::Serialize + Send + 'static,
    {
        let queue = self.queue.clone();
        let token = self.token.clone();
        let failed = self.failed_count.clone();

        tokio::spawn(async move {
            loop {
                let recv_result = tokio::select! {
                    biased;
                    () = token.cancelled() => break,
                    item = items.recv() => item,
                };
                let Some(item) = recv_result else {
                    break;
                };

                let json_payload = match item.payload.map(serde_json::to_value) {
                    Some(Ok(v)) => Some(v),
                    Some(Err(e)) => {
                        let error = format!("payload serialization failed: {e}");
                        tracing::error!(error = %error, "failed to serialize WorkItem payload");
                        failed.fetch_add(1, Ordering::SeqCst);
                        if let Err(send_err) = result_tx
                            .send(JobResult::failure(Uuid::new_v4(), error))
                            .await
                        {
                            tracing::warn!(error = %send_err, "failed to forward enqueuer failure");
                        }
                        continue;
                    }
                    None => None,
                };
                match queue.enqueue(item.job_type, item.priority, json_payload) {
                    Ok(id) => {
                        enqueued_ids
                            .lock()
                            .expect("enqueued_ids mutex poisoned")
                            .insert(id);
                        reporter.on_jobs_extended(1);
                    }
                    Err(e) => {
                        let error = format!("enqueue failed: {e}");
                        tracing::error!(error = %error, "Failed to enqueue job");
                        failed.fetch_add(1, Ordering::SeqCst);
                        if let Err(send_err) = result_tx
                            .send(JobResult::failure(Uuid::new_v4(), error))
                            .await
                        {
                            tracing::warn!(error = %send_err, "failed to forward enqueuer failure");
                        }
                    }
                }
            }
            producer_done_flag.store(true, Ordering::SeqCst);
        })
    }

    /// Spawn one worker task that loops on `queue.claim`, runs each claimed
    /// job, and exits cleanly when the enqueuer is done and the queue drains.
    fn spawn_worker<F, Fut>(&self, args: WorkerSpawnArgs<F>) -> JoinHandle<()>
    where
        F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
            + Send
            + 'static,
    {
        let WorkerSpawnArgs {
            producer_done_flag,
            execution_gate,
            processor,
            on_error,
            reporter,
            result_tx,
        } = args;
        let queue = self.queue.clone();
        let token = self.token.clone();
        let completed = self.completed_count.clone();
        let failed = self.failed_count.clone();
        let already_claimed = self.already_claimed_count.clone();
        let worker_id = format!(
            "{}-{}",
            self.config.worker_prefix,
            uuid::Uuid::new_v4().as_simple()
        );

        tokio::spawn(async move {
            // Wait on the execution gate (cancel-aware). The gate is fired
            // via `Notify::notify_waiters()`, which wakes every current
            // waiter — workers do not need to re-notify siblings.
            tokio::select! {
                biased;
                () = token.cancelled() => return,
                () = execution_gate.notified() => {}
            }

            loop {
                if token.is_cancelled() {
                    return;
                }

                // Claim the next pending job via the queue, which honors the
                // SQLite store's priority ordering. This is the same dispatch
                // order used by the batch path and by `--priority-by-date`.
                let claim_queue = queue.clone();
                let wid = worker_id.clone();
                let claim_result =
                    tokio::task::spawn_blocking(move || claim_queue.claim(&wid)).await;

                let job = match claim_result {
                    Ok(Ok(Some(job))) => job,
                    Ok(Ok(None)) => {
                        // No pending work right now. If the enqueuer is done,
                        // we're finished; otherwise sleep briefly and retry.
                        if producer_done_flag.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
                        continue;
                    }
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "Failed to claim next job");
                        // No job id to report against; keep looping. The token
                        // will eventually be cancelled if storage is broken.
                        tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Task join error during claim");
                        tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
                        continue;
                    }
                };

                let pre_failed = failed.load(Ordering::SeqCst);

                let ctx = WorkerContext {
                    queue: queue.clone(),
                    token: token.clone(),
                    completed: completed.clone(),
                    failed: failed.clone(),
                    already_claimed: already_claimed.clone(),
                    processor: processor.clone(),
                    reporter: reporter.clone(),
                    result_tx: result_tx.clone(),
                    worker_id: worker_id.clone(),
                    on_error,
                };
                run_claimed_job(job, ctx).await;

                // Fail-fast: if the on_error strategy is Fail and the failure
                // counter just incremented, this worker exits. `run_claimed_job`
                // itself cancels the token on failure under the Fail strategy,
                // so sibling workers will observe the cancellation on their
                // next loop iteration.
                if matches!(on_error, JobErrorStrategy::Fail)
                    && failed.load(Ordering::SeqCst) > pre_failed
                {
                    return;
                }
            }
        })
    }
}

/// Return job ids we enqueued in this `process_stream` run that are still in
/// `Pending` state. Used after streaming cancellation to cancel anything we
/// enqueued but no worker ever claimed. Scoping the sweep to ids we produced
/// avoids over-cancelling unrelated Pending rows that may exist in a
/// long-lived SQLite store (e.g. left behind by a prior crash).
fn collect_unstarted_enqueued_ids(
    queue: &Arc<crate::queue::JobQueue>,
    enqueued_ids: &Arc<Mutex<HashSet<Uuid>>>,
) -> Vec<Uuid> {
    let mut filters = voom_domain::storage::JobFilters::default();
    filters.status = Some(voom_domain::job::JobStatus::Pending);
    let pending = match queue.list_jobs(&filters) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!(error = %e, "failed to enumerate pending jobs for cancellation");
            return Vec::new();
        }
    };
    let ours = enqueued_ids.lock().expect("enqueued_ids mutex poisoned");
    pending
        .into_iter()
        .filter_map(|j| {
            if ours.contains(&j.id) {
                Some(j.id)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopReporter;
    use crate::queue::JobQueue;
    use crate::worker::{JobErrorStrategy, WorkerPool, WorkerPoolConfig};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;
    use tokio_util::sync::CancellationToken;
    use voom_domain::job::JobType;
    use voom_domain::storage::JobStorage;
    use voom_domain::test_support::InMemoryStore;

    fn fresh_pool() -> (WorkerPool, Arc<JobQueue>) {
        let store: Arc<dyn JobStorage> = Arc::new(InMemoryStore::new());
        let queue = Arc::new(JobQueue::new(store));
        let cfg = WorkerPoolConfig {
            max_workers: 2,
            worker_prefix: "test".into(),
        };
        let token = CancellationToken::new();
        (WorkerPool::new(queue.clone(), cfg, token), queue)
    }

    fn dummy_item() -> WorkItem<()> {
        WorkItem::new(JobType::Process, 100, None)
    }

    #[tokio::test]
    async fn process_stream_basic() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        for _ in 0..5 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| {
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await
        });

        // Give workers time to register on gate.notified() before opening it.
        // `Notify::notify_waiters` is edge-triggered; missed notifications are
        // lost, so callers MUST notify after waiters are ready.
        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_waiters();
        let results = handle.await.unwrap();

        assert_eq!(counter.load(Ordering::SeqCst), 5);
        assert_eq!(results.iter().filter(|r| r.is_success()).count(), 5);
    }

    #[tokio::test]
    async fn process_stream_respects_gate() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        for _ in 0..3 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| {
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        gate.notify_waiters();
        let results = handle.await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 3);
        assert_eq!(results.iter().filter(|r| r.is_success()).count(), 3);
    }

    #[tokio::test]
    async fn process_stream_cancellation_before_gate() {
        let store: Arc<dyn JobStorage> = Arc::new(InMemoryStore::new());
        let queue = Arc::new(JobQueue::new(store));
        let cfg = WorkerPoolConfig {
            max_workers: 2,
            worker_prefix: "test".into(),
        };
        let token = CancellationToken::new();
        let pool = WorkerPool::new(queue.clone(), cfg, token.clone());

        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        for _ in 0..3 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| {
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await
        });

        // Cancel before notifying the gate so no worker starts a claim.
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
        gate.notify_waiters();

        let _ = handle.await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        let mut filters = voom_domain::storage::JobFilters::default();
        filters.status = Some(voom_domain::job::JobStatus::Cancelled);
        let cancelled = queue.list_jobs(&filters).unwrap();
        assert_eq!(cancelled.len(), 3);
    }

    #[tokio::test]
    async fn process_stream_fail_fast() {
        let (pool, _queue) = fresh_pool();
        // Clone the cancellation token before moving the pool into the spawned
        // task so we can assert post-run that fail-fast actually cancelled.
        let pool_token = pool.token.clone();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        for _ in 0..5 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| {
                    let c = invocations_clone.clone();
                    async move {
                        let n = c.fetch_add(1, Ordering::SeqCst);
                        if n == 1 {
                            Err("boom".to_string())
                        } else {
                            Ok(None)
                        }
                    }
                },
                JobErrorStrategy::Fail,
                Arc::new(NoopReporter),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_waiters();
        let results = handle.await.unwrap();

        assert!(results.iter().any(|r| !r.is_success()));
        // Fail-fast must actually fire: the pool's cancellation token has to
        // be set. Without this assertion, the test would still pass even if
        // fail-fast didn't trigger.
        assert!(pool_token.is_cancelled(), "fail-fast must cancel the pool");
        // With max_workers = 2, at most two jobs should complete before the
        // failing one trips cancellation. Allow a small race tolerance.
        let successes = results.iter().filter(|r| r.is_success()).count();
        assert!(
            successes <= 2 + 1,
            "fail-fast should have stopped after a couple of jobs, saw {successes}"
        );
    }

    #[tokio::test]
    async fn process_stream_continue_after_failures() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        for _ in 0..4 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| {
                    let c = invocations_clone.clone();
                    async move {
                        let n = c.fetch_add(1, Ordering::SeqCst);
                        if n % 2 == 0 {
                            Ok(None)
                        } else {
                            Err("oops".into())
                        }
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_waiters();
        let results = handle.await.unwrap();

        assert_eq!(invocations.load(Ordering::SeqCst), 4);
        let failed = results.iter().filter(|r| !r.is_success()).count();
        let ok = results.iter().filter(|r| r.is_success()).count();
        assert_eq!(failed + ok, 4);
        assert!(failed > 0 && ok > 0);
    }

    #[tokio::test]
    async fn process_stream_extends_reporter_total() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let gate = Arc::new(Notify::new());

        struct CountReporter {
            extended: AtomicU64,
        }
        impl ProgressReporter for CountReporter {
            fn on_batch_start(&self, _total: usize) {}
            fn on_job_start(&self, _job: &voom_domain::job::Job) {}
            fn on_job_progress(&self, _id: Uuid, _p: f64, _m: Option<&str>) {}
            fn on_job_complete(&self, _id: Uuid, _ok: bool, _err: Option<&str>) {}
            fn on_batch_complete(&self, _c: u64, _f: u64) {}
            fn on_jobs_extended(&self, additional: usize) {
                self.extended.fetch_add(additional as u64, Ordering::SeqCst);
            }
        }
        let reporter: Arc<CountReporter> = Arc::new(CountReporter {
            extended: AtomicU64::new(0),
        });

        for _ in 0..6 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);

        let reporter_for_pool = reporter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |_job| async { Ok(None) },
                JobErrorStrategy::Continue,
                reporter_for_pool,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.notify_waiters();
        let _ = handle.await.unwrap();

        assert_eq!(reporter.extended.load(Ordering::SeqCst), 6);
    }

    /// Spec compliance: workers must dispatch jobs in SQLite priority order
    /// (lowest priority number first, then created_at). Regression test for
    /// the original `Vec::pop()` design which served jobs LIFO and bypassed
    /// the queue's priority ordering used by `--priority-by-date`.
    #[tokio::test]
    async fn process_stream_honors_priority_ordering() {
        let store: Arc<dyn JobStorage> = Arc::new(InMemoryStore::new());
        let queue = Arc::new(JobQueue::new(store));
        // Single worker so the dispatch order is deterministic.
        let cfg = WorkerPoolConfig {
            max_workers: 1,
            worker_prefix: "prio".into(),
        };
        let token = CancellationToken::new();
        let pool = WorkerPool::new(queue.clone(), cfg, token);

        let (tx, rx) = mpsc::channel::<WorkItem<i32>>(8);
        let gate = Arc::new(Notify::new());

        // Enqueue in mixed priority order. Expected dispatch order: 10, 50, 100, 200.
        for priority in [100, 50, 200, 10] {
            tx.send(WorkItem::new(JobType::Process, priority, Some(priority)))
                .await
                .unwrap();
        }
        drop(tx);

        let seen: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = seen.clone();
        let gate_for_task = gate.clone();

        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                gate_for_task,
                move |job| {
                    let seen = seen_clone.clone();
                    async move {
                        let payload = job.payload.as_ref().expect("priority payload missing");
                        let priority =
                            i32::try_from(payload.as_i64().expect("priority is integer"))
                                .expect("priority fits in i32");
                        seen.lock().expect("seen mutex").push(priority);
                        Ok(None)
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await
        });

        // Give the enqueuer a moment to insert all four jobs before opening
        // the gate. If we open the gate first, a worker could claim job 1
        // (priority 100) before jobs 2-4 are enqueued.
        tokio::time::sleep(Duration::from_millis(30)).await;
        gate.notify_waiters();
        let results = handle.await.unwrap();

        assert_eq!(results.iter().filter(|r| r.is_success()).count(), 4);
        let order = seen.lock().expect("seen mutex").clone();
        assert_eq!(
            order,
            vec![10, 50, 100, 200],
            "workers must claim jobs in SQLite priority order"
        );
    }
}
