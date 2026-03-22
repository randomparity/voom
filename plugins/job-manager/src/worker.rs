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
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub job_type: voom_domain::job::JobType,
    pub priority: i32,
    pub payload: Option<serde_json::Value>,
}

/// Configuration for the worker pool.
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
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Result of processing a single job.
#[derive(Debug)]
pub struct JobResult {
    pub job_id: Uuid,
    pub success: bool,
    pub error: Option<String>,
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

    /// Process a batch of work items concurrently.
    ///
    /// `items` is a list of work items to enqueue.
    /// `processor` is called for each claimed job and should return Ok(output) or Err(error).
    /// `on_error` controls behavior when a job fails.
    /// `reporter` is notified of progress updates.
    ///
    /// Returns a list of job results.
    #[tracing::instrument(skip(self, processor, reporter))]
    pub async fn process_batch<F, Fut>(
        &self,
        items: Vec<WorkItem>,
        processor: F,
        on_error: ErrorStrategy,
        reporter: Arc<dyn ProgressReporter>,
    ) -> Vec<JobResult>
    where
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
            match self
                .queue
                .enqueue(item.job_type, item.priority, item.payload)
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
                _ = self.token.cancelled() => break,
            };
            let queue = self.queue.clone();
            let token = self.token.clone();
            let completed = self.completed_count.clone();
            let failed = self.failed_count.clone();
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
                run_one_job(
                    job_id, queue, token, completed, failed, processor, reporter, result_tx,
                    worker_id, on_error,
                )
                .await;
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

/// Execute a single job: claim it, run the processor, and record the result.
#[allow(clippy::too_many_arguments)]
async fn run_one_job<F, Fut>(
    job_id: Uuid,
    queue: Arc<JobQueue>,
    token: CancellationToken,
    completed: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
    processor: Arc<F>,
    reporter: Arc<dyn ProgressReporter>,
    result_tx: mpsc::Sender<JobResult>,
    worker_id: String,
    on_error: ErrorStrategy,
) where
    F: Fn(voom_domain::job::Job) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<Option<serde_json::Value>, String>>
        + Send
        + 'static,
{
    if token.is_cancelled() {
        failed.fetch_add(1, Ordering::SeqCst);
        let _ = result_tx
            .send(JobResult {
                job_id,
                success: false,
                error: Some("cancelled".into()),
            })
            .await;
        return;
    }

    // Claim the specific job by ID (blocking storage call)
    let queue_claim = queue.clone();
    let wid = worker_id.clone();
    let jid = job_id;
    let job = match tokio::task::spawn_blocking(move || queue_claim.claim_by_id(&jid, &wid)).await {
        Ok(Ok(Some(job))) => job,
        Ok(Ok(None)) => {
            // Job was claimed by another worker — count as completed
            completed.fetch_add(1, Ordering::SeqCst);
            let _ = result_tx
                .send(JobResult {
                    job_id,
                    success: true,
                    error: None,
                })
                .await;
            return;
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, "Failed to claim job");
            failed.fetch_add(1, Ordering::SeqCst);
            let _ = result_tx
                .send(JobResult {
                    job_id,
                    success: false,
                    error: Some(format!("failed to claim: {e}")),
                })
                .await;
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "Task join error");
            failed.fetch_add(1, Ordering::SeqCst);
            let _ = result_tx
                .send(JobResult {
                    job_id,
                    success: false,
                    error: Some(format!("task join error: {e}")),
                })
                .await;
            return;
        }
    };

    // Re-check cancellation after claiming (closes race with ErrorStrategy::Fail)
    if token.is_cancelled() {
        let q = queue.clone();
        let jid = job.id;
        if let Err(e) = tokio::task::spawn_blocking(move || q.cancel(&jid)).await {
            tracing::error!(error = %e, "failed to mark job as cancelled");
        }
        failed.fetch_add(1, Ordering::SeqCst);
        let _ = result_tx
            .send(JobResult {
                job_id,
                success: false,
                error: Some("cancelled".into()),
            })
            .await;
        return;
    }

    let job_id = job.id;
    reporter.on_job_start(&job);

    match processor(job).await {
        Ok(output) => {
            let q = queue.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || q.complete(&job_id, output)).await {
                tracing::error!(job_id = %job_id, error = %e, "failed to mark job as complete");
            }
            completed.fetch_add(1, Ordering::SeqCst);
            reporter.on_job_complete(job_id, true, None);

            if let Err(e) = result_tx
                .send(JobResult {
                    job_id,
                    success: true,
                    error: None,
                })
                .await
            {
                tracing::warn!(job_id = %job_id, error = %e, "failed to send job result");
            }
        }
        Err(error) => {
            let q = queue.clone();
            let err = error.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || q.fail(&job_id, err)).await {
                tracing::error!(job_id = %job_id, error = %e, "failed to mark job as failed");
            }
            failed.fetch_add(1, Ordering::SeqCst);
            reporter.on_job_complete(job_id, false, Some(&error));

            if let Err(e) = result_tx
                .send(JobResult {
                    job_id,
                    success: false,
                    error: Some(error),
                })
                .await
            {
                tracing::warn!(job_id = %job_id, error = %e, "failed to send job result");
            }

            if on_error == ErrorStrategy::Fail {
                token.cancel();
            }
        }
    }
}

/// How to handle errors during batch processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStrategy {
    /// Stop all processing on first error.
    Fail,
    /// Continue processing remaining items, collecting all errors.
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

        let items: Vec<_> = (0..5)
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
            ErrorStrategy::Continue,
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
            ErrorStrategy::Continue,
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

        let items = vec![
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
            ErrorStrategy::Fail,
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
}
