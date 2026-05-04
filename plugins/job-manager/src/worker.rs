//! Worker pool for concurrent job processing using tokio tasks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::progress::ProgressReporter;
use crate::queue::JobQueue;

/// A unit of work to be enqueued and processed by the worker pool.
///
/// The payload type `P` defaults to `()` for convenience in tests and callers
/// that don't need a payload.  When `P` implements `Serialize`, the worker pool
/// converts it to `serde_json::Value` at enqueue time, keeping the caller's
/// side free of manual serialization.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct WorkItem<P = ()> {
    pub job_type: voom_domain::job::JobType,
    pub priority: i32,
    pub payload: Option<P>,
}

impl<P> WorkItem<P> {
    /// Create a new work item with the given job type, priority, and optional payload.
    #[must_use]
    pub fn new(job_type: voom_domain::job::JobType, priority: i32, payload: Option<P>) -> Self {
        Self {
            job_type,
            priority,
            payload,
        }
    }
}

/// Configuration for the worker pool.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct WorkerPoolConfig {
    /// Maximum number of concurrent workers. 0 = number of CPUs.
    pub max_workers: usize,
    /// Worker ID prefix for identification.
    pub worker_prefix: String,
}

impl Default for WorkerPoolConfig {
    fn default() -> Self {
        Self {
            max_workers: 0,
            worker_prefix: "worker".to_string(),
        }
    }
}

impl WorkerPoolConfig {
    /// Resolve the actual worker count (0 means use CPU count).
    #[must_use]
    pub fn effective_workers(&self) -> usize {
        if self.max_workers == 0 {
            num_cpus()
        } else {
            self.max_workers
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(4)
}

/// Outcome of a single job execution.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobOutcome {
    /// The job was processed to completion.
    Success,
    /// The job's processor returned an error.
    Failure(String),
    /// Another worker claimed the job before this one could. Neither success
    /// nor failure — the caller should not count it toward either total, and
    /// `JobErrorStrategy::Fail` should NOT trigger on this outcome.
    AlreadyClaimed,
}

impl JobOutcome {
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure(_))
    }

    #[must_use]
    pub fn is_already_claimed(&self) -> bool {
        matches!(self, Self::AlreadyClaimed)
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failure(e) => Some(e.as_str()),
            _ => None,
        }
    }
}

/// Result of processing a single job.
#[non_exhaustive]
#[derive(Debug)]
pub struct JobResult {
    pub job_id: Uuid,
    pub outcome: JobOutcome,
}

impl JobResult {
    #[must_use]
    pub fn success(job_id: Uuid) -> Self {
        Self {
            job_id,
            outcome: JobOutcome::Success,
        }
    }

    #[must_use]
    pub fn failure(job_id: Uuid, error: String) -> Self {
        Self {
            job_id,
            outcome: JobOutcome::Failure(error),
        }
    }

    #[must_use]
    pub fn already_claimed(job_id: Uuid) -> Self {
        Self {
            job_id,
            outcome: JobOutcome::AlreadyClaimed,
        }
    }

    /// Backward-compatible accessor. True only for `Success`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.outcome.is_success()
    }

    /// True only for `AlreadyClaimed`.
    #[must_use]
    pub fn is_already_claimed(&self) -> bool {
        self.outcome.is_already_claimed()
    }

    /// Backward-compatible accessor. Returns the error string for `Failure`,
    /// `None` otherwise.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.outcome.error()
    }
}

/// A batch of work items to process concurrently.
///
/// The worker pool manages concurrency via a semaphore, spawning tokio tasks
/// up to `max_workers` concurrently. Each work item is processed by a
/// user-provided async function.
pub struct WorkerPool {
    config: WorkerPoolConfig,
    queue: Arc<JobQueue>,
    token: CancellationToken,
    completed_count: Arc<AtomicU64>,
    failed_count: Arc<AtomicU64>,
    already_claimed_count: Arc<AtomicU64>,
}

impl WorkerPool {
    #[must_use]
    pub fn new(queue: Arc<JobQueue>, config: WorkerPoolConfig, token: CancellationToken) -> Self {
        Self {
            config,
            queue,
            token,
            completed_count: Arc::new(AtomicU64::new(0)),
            failed_count: Arc::new(AtomicU64::new(0)),
            already_claimed_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    #[must_use]
    pub fn completed_count(&self) -> u64 {
        self.completed_count.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn failed_count(&self) -> u64 {
        self.failed_count.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn already_claimed_count(&self) -> u64 {
        self.already_claimed_count.load(Ordering::SeqCst)
    }

    /// Process a batch of work items concurrently.
    ///
    /// `items` is a list of work items to enqueue.
    /// `processor` is called for each claimed job and should return Ok(output) or Err(error).
    /// `on_error` controls behavior when a job fails.
    /// `reporter` is notified of progress updates.
    ///
    /// Returns a list of job results.
    #[tracing::instrument(skip(self, items, processor, reporter))]
    pub async fn process_batch<P, F, Fut>(
        &self,
        items: Vec<WorkItem<P>>,
        processor: F,
        on_error: JobErrorStrategy,
        reporter: Arc<dyn ProgressReporter>,
    ) -> Vec<JobResult>
    where
        P: serde::Serialize,
        F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
            + Send
            + 'static,
    {
        let effective_workers = self.config.effective_workers();
        let semaphore = Arc::new(Semaphore::new(effective_workers));
        let processor = Arc::new(processor);

        tracing::info!(
            workers = effective_workers,
            jobs = items.len(),
            "Starting worker pool"
        );

        let mut job_ids = Vec::with_capacity(items.len());
        for item in items {
            let json_payload = match item.payload.map(serde_json::to_value) {
                Some(Ok(v)) => Some(v),
                Some(Err(e)) => {
                    tracing::error!(error = %e, "failed to serialize WorkItem payload");
                    continue;
                }
                None => None,
            };
            match self
                .queue
                .enqueue(item.job_type, item.priority, json_payload)
            {
                Ok(id) => job_ids.push(id),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to enqueue job");
                }
            }
        }

        reporter.on_batch_start(job_ids.len());

        let (result_tx, mut result_rx) = mpsc::channel::<JobResult>(job_ids.len().max(1));
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        for job_id in job_ids {
            let permit = tokio::select! {
                p = semaphore.clone().acquire_owned() => p.expect("semaphore not closed"),
                () = self.token.cancelled() => break,
            };
            let queue = self.queue.clone();
            let token = self.token.clone();
            let completed = self.completed_count.clone();
            let failed = self.failed_count.clone();
            let already_claimed = self.already_claimed_count.clone();
            let processor = processor.clone();
            let result_tx = result_tx.clone();
            let reporter = reporter.clone();
            let worker_id = format!(
                "{}-{}",
                self.config.worker_prefix,
                uuid::Uuid::new_v4().as_simple()
            );

            let handle = tokio::spawn(async move {
                let _permit = permit;
                let ctx = WorkerContext {
                    queue,
                    token,
                    completed,
                    failed,
                    already_claimed,
                    processor,
                    reporter,
                    result_tx,
                    worker_id,
                    on_error,
                };
                run_one_job(job_id, ctx).await;
            });

            handles.push(handle);
        }

        drop(result_tx);

        let mut results = Vec::new();
        while let Some(result) = result_rx.recv().await {
            results.push(result);
        }

        for handle in handles {
            if let Err(e) = handle.await {
                tracing::error!(error = %e, "worker task join error");
            }
        }

        reporter.on_batch_complete(
            self.completed_count.load(Ordering::SeqCst),
            self.failed_count.load(Ordering::SeqCst),
        );

        results
    }
}

/// Shared context passed to each worker task.
struct WorkerContext<F> {
    queue: Arc<JobQueue>,
    token: CancellationToken,
    completed: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
    already_claimed: Arc<AtomicU64>,
    processor: Arc<F>,
    reporter: Arc<dyn ProgressReporter>,
    result_tx: mpsc::Sender<JobResult>,
    worker_id: String,
    on_error: JobErrorStrategy,
}

/// Increment the failed counter and send a `JobResult` describing the failure.
///
/// Consolidates the 5 near-identical failure-exit paths in `run_one_job` so
/// the caller can simply `send_failure(...).await; return;` at each.
async fn send_failure<F>(ctx: &WorkerContext<F>, job_id: Uuid, error: String) {
    ctx.failed.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = ctx.result_tx.send(JobResult::failure(job_id, error)).await {
        tracing::warn!(job_id = %job_id, error = %e, "failed to send failure JobResult");
    }
}

/// Increment the already-claimed counter and emit a `JobResult::AlreadyClaimed`.
///
/// Used when `claim_by_id` returns `None` because another worker (or a
/// previous run) already owns the job. NOT a failure: the job still exists
/// and will be processed by whichever worker holds the claim.
async fn send_claim_race<F>(ctx: &WorkerContext<F>, job_id: Uuid) {
    ctx.already_claimed.fetch_add(1, Ordering::SeqCst);
    if let Err(e) = ctx.result_tx.send(JobResult::already_claimed(job_id)).await {
        tracing::warn!(job_id = %job_id, error = %e, "failed to send already-claimed JobResult");
    }
}

/// Execute a single job: claim it, run the processor, and record the result.
async fn run_one_job<F, Fut>(job_id: Uuid, ctx: WorkerContext<F>)
where
    F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
        + Send
        + 'static,
{
    if ctx.token.is_cancelled() {
        send_failure(&ctx, job_id, "cancelled".into()).await;
        return;
    }

    // Claim the specific job by ID (blocking storage call)
    let queue_claim = ctx.queue.clone();
    let wid = ctx.worker_id.clone();
    let jid = job_id;
    let job = match tokio::task::spawn_blocking(move || queue_claim.claim_by_id(&jid, &wid)).await {
        Ok(Ok(Some(job))) => job,
        Ok(Ok(None)) => {
            // Lost the claim race — another worker (or a previous run) already
            // owns this job. NOT a failure: the job still exists and the other
            // worker will process it. Report distinctly so JobErrorStrategy::Fail
            // does not cancel the batch for normal racing.
            tracing::debug!(
                %job_id,
                worker = %ctx.worker_id,
                "job already claimed by another worker"
            );
            send_claim_race(&ctx, job_id).await;
            return;
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Failed to claim job");
            send_failure(&ctx, job_id, format!("failed to claim: {e}")).await;
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "Task join error");
            send_failure(&ctx, job_id, format!("task join error: {e}")).await;
            return;
        }
    };

    // Re-check cancellation after claiming (closes race with JobErrorStrategy::Fail)
    if ctx.token.is_cancelled() {
        let q = ctx.queue.clone();
        let jid = job.id;
        if let Err(e) = tokio::task::spawn_blocking(move || q.cancel(&jid)).await {
            tracing::error!(error = %e, "failed to mark job as cancelled");
        }
        send_failure(&ctx, job_id, "cancelled".into()).await;
        return;
    }

    let job_id = job.id;
    ctx.reporter.on_job_start(&job);

    match (ctx.processor)(job).await {
        Ok(output) => {
            let q = ctx.queue.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || q.complete(&job_id, output)).await {
                tracing::error!(job_id = %job_id, error = %e, "failed to mark job as complete");
            }
            ctx.completed.fetch_add(1, Ordering::SeqCst);
            ctx.reporter.on_job_complete(job_id, true, None);

            if let Err(e) = ctx.result_tx.send(JobResult::success(job_id)).await {
                tracing::warn!(job_id = %job_id, error = %e, "failed to send job result");
            }
        }
        Err(error) => {
            let q = ctx.queue.clone();
            let err = error.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || q.fail(&job_id, err)).await {
                tracing::error!(job_id = %job_id, error = %e, "failed to mark job as failed");
            }
            ctx.reporter.on_job_complete(job_id, false, Some(&error));

            // send_failure increments `failed` and dispatches the JobResult.
            let strategy = ctx.on_error;
            let token = ctx.token.clone();
            send_failure(&ctx, job_id, error).await;

            if strategy == JobErrorStrategy::Fail {
                token.cancel();
            }
        }
    }
}

/// How to handle errors during batch processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobErrorStrategy {
    /// Stop all processing on first error.
    Fail,
    /// Skip the failed item and continue processing remaining items.
    /// The failed item is recorded but does not halt the batch.
    Skip,
    /// Continue processing all remaining items, collecting all errors.
    /// Failed items are recorded but do not halt the batch.
    Continue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopReporter;
    use std::sync::atomic::AtomicU32;
    use tokio_util::sync::CancellationToken;
    use voom_domain::test_support::InMemoryStore;

    fn test_queue() -> Arc<JobQueue> {
        let store = Arc::new(InMemoryStore::new());
        Arc::new(JobQueue::new(store))
    }

    #[tokio::test]
    async fn test_worker_pool_basic() {
        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 2,
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let items: Vec<WorkItem> = (0..5)
            .map(|i| WorkItem {
                job_type: voom_domain::job::JobType::Custom(format!("task-{i}")),
                priority: 100,
                payload: None,
            })
            .collect();

        pool.process_batch(
            items,
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
        .await;

        assert_eq!(counter.load(Ordering::SeqCst), 5);
        assert_eq!(pool.completed_count(), 5);
        assert_eq!(pool.failed_count(), 0);
    }

    #[tokio::test]
    async fn test_worker_pool_with_failures() {
        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 2,
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let items: Vec<_> = (0..4)
            .map(|i| WorkItem {
                job_type: voom_domain::job::JobType::Custom(format!("task-{i}")),
                priority: 100,
                payload: Some(serde_json::json!({"i": i})),
            })
            .collect();

        pool.process_batch(
            items,
            |job| async move {
                let payload = job.payload.as_ref().unwrap();
                let i = payload["i"].as_u64().unwrap();
                if i % 2 == 0 {
                    Err(format!("task {i} failed"))
                } else {
                    Ok(None)
                }
            },
            JobErrorStrategy::Continue,
            Arc::new(NoopReporter),
        )
        .await;

        assert_eq!(pool.completed_count(), 2);
        assert_eq!(pool.failed_count(), 2);
    }

    #[tokio::test]
    async fn test_worker_pool_fail_fast() {
        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 1, // sequential to make behavior deterministic
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let items: Vec<WorkItem> = vec![
            WorkItem {
                job_type: voom_domain::job::JobType::Custom("fail".into()),
                priority: 50,
                payload: None,
            }, // claimed first (lower priority)
            WorkItem {
                job_type: voom_domain::job::JobType::Custom("ok".into()),
                priority: 100,
                payload: None,
            },
        ];

        pool.process_batch(
            items,
            |job| async move {
                if job.job_type == voom_domain::job::JobType::Custom("fail".into()) {
                    Err("boom".into())
                } else {
                    Ok(None)
                }
            },
            JobErrorStrategy::Fail,
            Arc::new(NoopReporter),
        )
        .await;

        assert!(pool.failed_count() >= 1);
    }

    #[test]
    fn test_effective_workers() {
        let config = WorkerPoolConfig {
            max_workers: 4,
            ..Default::default()
        };
        assert_eq!(config.effective_workers(), 4);

        let config = WorkerPoolConfig {
            max_workers: 0,
            ..Default::default()
        };
        assert!(config.effective_workers() >= 1);
    }

    #[test]
    fn test_cancellation() {
        let queue = test_queue();
        let pool = WorkerPool::new(queue, WorkerPoolConfig::default(), CancellationToken::new());
        assert!(!pool.is_cancelled());
        pool.cancel();
        assert!(pool.is_cancelled());
    }

    #[tokio::test]
    async fn test_error_strategy_skip_continues_after_failure() {
        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 1, // sequential for determinism
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let items: Vec<_> = (0..5)
            .map(|i| WorkItem {
                job_type: voom_domain::job::JobType::Custom(format!("task-{i}")),
                priority: 100,
                payload: Some(serde_json::json!({"i": i})),
            })
            .collect();

        let results = pool
            .process_batch(
                items,
                move |job| {
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        let payload = job.payload.as_ref().unwrap();
                        let i = payload["i"].as_u64().unwrap();
                        if i == 0 {
                            Err("first item fails".into())
                        } else {
                            Ok(None)
                        }
                    }
                },
                JobErrorStrategy::Skip,
                Arc::new(NoopReporter),
            )
            .await;

        // All 5 items were attempted despite the first one failing
        assert_eq!(counter.load(Ordering::SeqCst), 5);
        // 4 succeeded, 1 failed
        assert_eq!(pool.completed_count(), 4);
        assert_eq!(pool.failed_count(), 1);
        // Results contain both successes and failures
        let failures: Vec<_> = results.iter().filter(|r| r.outcome.is_failure()).collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].error(), Some("first item fails"));
    }

    #[tokio::test]
    async fn test_error_strategy_continue_attempts_all_items() {
        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 1,
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        // Create 6 items where every other one fails
        let items: Vec<_> = (0..6)
            .map(|i| WorkItem {
                job_type: voom_domain::job::JobType::Custom(format!("task-{i}")),
                priority: 100,
                payload: Some(serde_json::json!({"i": i})),
            })
            .collect();

        let results = pool
            .process_batch(
                items,
                move |job| {
                    let c = counter_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        let payload = job.payload.as_ref().unwrap();
                        let i = payload["i"].as_u64().unwrap();
                        if i % 2 == 0 {
                            Err(format!("task-{i} failed"))
                        } else {
                            Ok(Some(serde_json::json!({"result": i})))
                        }
                    }
                },
                JobErrorStrategy::Continue,
                Arc::new(NoopReporter),
            )
            .await;

        // All 6 items were attempted
        assert_eq!(counter.load(Ordering::SeqCst), 6);
        assert_eq!(pool.completed_count(), 3);
        assert_eq!(pool.failed_count(), 3);
        // All 6 results are present
        assert_eq!(results.len(), 6);
        let successes: Vec<_> = results.iter().filter(|r| r.is_success()).collect();
        let failures: Vec<_> = results.iter().filter(|r| r.outcome.is_failure()).collect();
        assert_eq!(successes.len(), 3);
        assert_eq!(failures.len(), 3);
    }

    #[tokio::test]
    async fn test_concurrent_execution_multiple_workers() {
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;

        let queue = test_queue();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 4,
                ..Default::default()
            },
            CancellationToken::new(),
        );

        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let active_count = Arc::new(AtomicUsize::new(0));
        let total_processed = Arc::new(AtomicU32::new(0));

        let max_concurrent_clone = max_concurrent.clone();
        let active_count_clone = active_count.clone();
        let total_processed_clone = total_processed.clone();

        let items: Vec<WorkItem> = (0..8)
            .map(|i| WorkItem {
                job_type: voom_domain::job::JobType::Custom(format!("concurrent-{i}")),
                priority: 100,
                payload: None,
            })
            .collect();

        pool.process_batch(
            items,
            move |_job| {
                let max_c = max_concurrent_clone.clone();
                let active = active_count_clone.clone();
                let total = total_processed_clone.clone();
                async move {
                    // Track concurrent execution
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    // Update max concurrency seen
                    max_c.fetch_max(current, Ordering::SeqCst);

                    // Small delay to allow concurrent tasks to overlap
                    tokio::time::sleep(Duration::from_millis(20)).await;

                    active.fetch_sub(1, Ordering::SeqCst);
                    total.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                }
            },
            JobErrorStrategy::Continue,
            Arc::new(NoopReporter),
        )
        .await;

        // All 8 items were processed
        assert_eq!(total_processed.load(Ordering::SeqCst), 8);
        assert_eq!(pool.completed_count(), 8);
        assert_eq!(pool.failed_count(), 0);
        // With 4 workers and 8 items with a 20ms delay, we expect some concurrency
        // (at least 2 tasks running simultaneously)
        assert!(
            max_concurrent.load(Ordering::SeqCst) >= 2,
            "Expected concurrent execution with max_workers=4, but max concurrency was {}",
            max_concurrent.load(Ordering::SeqCst)
        );
    }

    /// Storage wrapper that always returns `Ok(None)` from `claim_job_by_id`,
    /// simulating a job already claimed by a different worker. All other
    /// operations delegate to an inner `InMemoryStore`.
    struct AlwaysClaimedStore {
        inner: Arc<InMemoryStore>,
    }

    impl AlwaysClaimedStore {
        fn new() -> Self {
            Self {
                inner: Arc::new(InMemoryStore::new()),
            }
        }
    }

    impl voom_domain::storage::JobStorage for AlwaysClaimedStore {
        fn create_job(&self, job: &voom_domain::job::Job) -> voom_domain::errors::Result<Uuid> {
            self.inner.create_job(job)
        }

        fn job(&self, id: &Uuid) -> voom_domain::errors::Result<Option<voom_domain::job::Job>> {
            self.inner.job(id)
        }

        fn update_job(
            &self,
            id: &Uuid,
            update: &voom_domain::job::JobUpdate,
        ) -> voom_domain::errors::Result<()> {
            self.inner.update_job(id, update)
        }

        fn claim_next_job(
            &self,
            worker_id: &str,
        ) -> voom_domain::errors::Result<Option<voom_domain::job::Job>> {
            self.inner.claim_next_job(worker_id)
        }

        fn claim_job_by_id(
            &self,
            _job_id: &Uuid,
            _worker_id: &str,
        ) -> voom_domain::errors::Result<Option<voom_domain::job::Job>> {
            // Simulate the race: every claim attempt loses to another worker.
            Ok(None)
        }

        fn list_jobs(
            &self,
            filters: &voom_domain::storage::JobFilters,
        ) -> voom_domain::errors::Result<Vec<voom_domain::job::Job>> {
            self.inner.list_jobs(filters)
        }

        fn count_jobs_by_status(
            &self,
        ) -> voom_domain::errors::Result<Vec<(voom_domain::job::JobStatus, u64)>> {
            self.inner.count_jobs_by_status()
        }

        fn delete_jobs(
            &self,
            status: Option<voom_domain::job::JobStatus>,
        ) -> voom_domain::errors::Result<u64> {
            self.inner.delete_jobs(status)
        }

        fn prune_old_jobs(
            &self,
            policy: voom_domain::storage::RetentionPolicy,
        ) -> voom_domain::errors::Result<voom_domain::storage::PruneReport> {
            self.inner.prune_old_jobs(policy)
        }

        fn count_old_jobs(
            &self,
            policy: voom_domain::storage::RetentionPolicy,
        ) -> voom_domain::errors::Result<voom_domain::storage::PruneReport> {
            self.inner.count_old_jobs(policy)
        }

        fn oldest_job_created_at(
            &self,
        ) -> voom_domain::errors::Result<Option<chrono::DateTime<chrono::Utc>>> {
            self.inner.oldest_job_created_at()
        }
    }

    #[tokio::test]
    async fn claim_race_does_not_trigger_fail_strategy() {
        let store: Arc<dyn voom_domain::storage::JobStorage> = Arc::new(AlwaysClaimedStore::new());
        let queue = Arc::new(JobQueue::new(store));
        let token = CancellationToken::new();
        let pool = WorkerPool::new(
            queue.clone(),
            WorkerPoolConfig {
                max_workers: 1,
                ..Default::default()
            },
            token.clone(),
        );

        let items: Vec<WorkItem> = vec![WorkItem {
            job_type: voom_domain::job::JobType::Custom("racey".into()),
            priority: 100,
            payload: None,
        }];

        let processor_called = Arc::new(AtomicU32::new(0));
        let processor_called_clone = processor_called.clone();

        let results = pool
            .process_batch(
                items,
                move |_job| {
                    let c = processor_called_clone.clone();
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    }
                },
                JobErrorStrategy::Fail,
                Arc::new(NoopReporter),
            )
            .await;

        assert_eq!(
            processor_called.load(Ordering::SeqCst),
            0,
            "processor must not run when claim is lost"
        );
        assert!(
            !token.is_cancelled(),
            "Fail strategy should NOT fire on claim race"
        );
        assert_eq!(
            pool.completed_count(),
            0,
            "no job should count as completed"
        );
        assert_eq!(pool.failed_count(), 0, "no job should count as failed");
        assert_eq!(
            pool.already_claimed_count(),
            1,
            "claim race should be counted separately"
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, JobOutcome::AlreadyClaimed);
    }
}
