//! Streaming entry point for `WorkerPool`: consume `WorkItem`s from an
//! `mpsc::Receiver`, enqueue them into the SQLite-backed `JobQueue`, and
//! claim/process them concurrently while an `execution_gate` is held open.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::progress::ProgressReporter;
use crate::worker::{
    JobErrorStrategy, JobResult, WorkItem, WorkerContext, WorkerPool, cancel_unstarted_jobs,
    run_one_job,
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
    pending: Arc<Mutex<Vec<Uuid>>>,
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
    /// Cancellation: the pool's internal token (passed via [`WorkerPool::new`])
    /// is the shared signal. When cancelled, the enqueuer stops draining the
    /// receiver, the remaining queued-but-unstarted jobs are cancelled via
    /// [`cancel_unstarted_jobs`], and workers exit after their current job.
    ///
    /// The `producer_done` [`Notify`] parameter is accepted for API symmetry
    /// with upstream callers that want to signal "no more items" explicitly,
    /// but it is intentionally not consumed: dropping the `tx` end of `items`
    /// is the canonical "producer done" signal and is sufficient on its own.
    /// Internally we flip an `AtomicBool` when the receiver returns `None`,
    /// avoiding the `Notify::notified()` consumption pitfall (notifications
    /// don't latch — a poll loop on `notified()` would race itself).
    #[tracing::instrument(skip(self, items, producer_done, execution_gate, processor, reporter))]
    pub async fn process_stream<P, F, Fut>(
        &self,
        items: mpsc::Receiver<WorkItem<P>>,
        producer_done: Arc<Notify>,
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
        let _ = producer_done; // intentionally unused: see doc comment above.

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

        // Tracks job IDs that have been enqueued and not yet been claimed by
        // a worker. Workers pop one before invoking `run_one_job`; on
        // cancellation, leftover IDs are sent to `cancel_unstarted_jobs`.
        let pending: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));

        let (result_tx, mut result_rx) = mpsc::channel::<JobResult>(effective_workers.max(1) * 4);

        // --- Enqueuer task --------------------------------------------------
        let enqueuer_handle = self.spawn_enqueuer(
            items,
            producer_done_flag.clone(),
            pending.clone(),
            reporter.clone(),
            result_tx.clone(),
        );

        // --- Worker tasks ---------------------------------------------------
        let mut worker_handles: Vec<JoinHandle<()>> = Vec::with_capacity(effective_workers);
        for _ in 0..effective_workers {
            let handle = self.spawn_worker(WorkerSpawnArgs {
                producer_done_flag: producer_done_flag.clone(),
                pending: pending.clone(),
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

        // Wait for the enqueuer to exit before draining results: it owns the
        // `pending` list and we want it fully populated before cancel cleanup.
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

        // If we were cancelled, cancel anything still pending (i.e. enqueued
        // but never claimed). On the happy path `pending` is empty here.
        if self.token.is_cancelled() {
            let leftover: Vec<Uuid> = {
                let mut guard = pending.lock().expect("pending mutex poisoned");
                std::mem::take(&mut *guard)
            };
            cancel_unstarted_jobs(self.queue.clone(), leftover).await;
        }

        reporter.on_batch_complete(
            self.completed_count.load(Ordering::SeqCst),
            self.failed_count.load(Ordering::SeqCst),
        );

        results
    }

    /// Spawn the single enqueuer task that drains `items` into the job queue,
    /// tracks pending IDs, and flips `producer_done_flag` on completion.
    fn spawn_enqueuer<P>(
        &self,
        mut items: mpsc::Receiver<WorkItem<P>>,
        producer_done_flag: Arc<AtomicBool>,
        pending: Arc<Mutex<Vec<Uuid>>>,
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
                        pending.lock().expect("pending mutex poisoned").push(id);
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
    /// job, and exits cleanly when the enqueuer is done and no jobs remain.
    fn spawn_worker<F, Fut>(&self, args: WorkerSpawnArgs<F>) -> JoinHandle<()>
    where
        F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
            + Send
            + 'static,
    {
        let WorkerSpawnArgs {
            producer_done_flag,
            pending,
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

                // Pop the next job ID from the pending list. Workers consume
                // IDs the enqueuer has produced; `run_one_job` then claims by
                // ID and runs the processor.
                let next_id = pending.lock().expect("pending mutex poisoned").pop();

                let Some(job_id) = next_id else {
                    // No pending work. If the enqueuer is done, we're
                    // finished; otherwise sleep briefly and re-check.
                    if producer_done_flag.load(Ordering::SeqCst) {
                        return;
                    }
                    tokio::time::sleep(CLAIM_POLL_INTERVAL).await;
                    continue;
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
                run_one_job(job_id, ctx).await;

                // Fail-fast: if the on_error strategy is Fail and the failure
                // counter just incremented, this worker exits. `run_one_job`
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopReporter;
    use crate::queue::JobQueue;
    use crate::worker::{JobErrorStrategy, WorkerPool, WorkerPoolConfig};
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
        let producer_done = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        for _ in 0..5 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);
        producer_done.notify_waiters();

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
        let producer_done = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        for _ in 0..3 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);
        producer_done.notify_waiters();

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();

        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
        let producer_done = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        for _ in 0..3 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);
        producer_done.notify_waiters();

        let counter = Arc::new(AtomicU64::new(0));
        let counter_clone = counter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let producer_done = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        for _ in 0..5 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);
        producer_done.notify_waiters();

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
        let successes = results.iter().filter(|r| r.is_success()).count();
        assert!(
            successes < 5,
            "should not have run all five jobs to completion"
        );
    }

    #[tokio::test]
    async fn process_stream_continue_after_failures() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let producer_done = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());

        for _ in 0..4 {
            tx.send(dummy_item()).await.unwrap();
        }
        drop(tx);
        producer_done.notify_waiters();

        let invocations = Arc::new(AtomicU64::new(0));
        let invocations_clone = invocations.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
    async fn process_stream_enqueue_serialization_error() {
        // Intentionally a no-op: there is no easy way to produce a payload
        // type that fails to serialize except via a custom Serialize impl,
        // and the same code path is already covered by the equivalent test
        // in `worker.rs` (`payload_serialization_failure_returns_failed_result_and_continues`).
    }

    #[tokio::test]
    async fn process_stream_extends_reporter_total() {
        let (pool, _queue) = fresh_pool();
        let (tx, rx) = mpsc::channel::<WorkItem<()>>(8);
        let producer_done = Arc::new(Notify::new());
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
        producer_done.notify_waiters();

        let reporter_for_pool = reporter.clone();
        let gate_for_task = gate.clone();
        let handle = tokio::spawn(async move {
            pool.process_stream(
                rx,
                producer_done,
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
}
