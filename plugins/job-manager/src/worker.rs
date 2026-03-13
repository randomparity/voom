//! Worker pool for concurrent job processing using tokio tasks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::progress::ProgressReporter;
use crate::queue::JobQueue;

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
    cancelled: Arc<AtomicBool>,
    completed_count: Arc<AtomicU64>,
    failed_count: Arc<AtomicU64>,
}

impl WorkerPool {
    #[must_use]
    pub fn new(queue: Arc<JobQueue>, config: WorkerPoolConfig) -> Self {
        Self {
            config,
            queue,
            cancelled: Arc::new(AtomicBool::new(false)),
            completed_count: Arc::new(AtomicU64::new(0)),
            failed_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Signal cancellation to all workers.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Check if cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Get the number of completed jobs.
    #[must_use]
    pub fn completed_count(&self) -> u64 {
        self.completed_count.load(Ordering::SeqCst)
    }

    /// Get the number of failed jobs.
    #[must_use]
    pub fn failed_count(&self) -> u64 {
        self.failed_count.load(Ordering::SeqCst)
    }

    /// Process a batch of work items concurrently.
    ///
    /// `items` is a list of (`job_type`, priority, payload) tuples to enqueue.
    /// `processor` is called for each claimed job and should return Ok(output) or Err(error).
    /// `on_error` controls behavior when a job fails.
    /// `reporter` is notified of progress updates.
    ///
    /// Returns a list of job results.
    #[tracing::instrument(skip(self, processor, reporter))]
    pub async fn process_batch<F, Fut>(
        &self,
        items: Vec<(String, i32, Option<serde_json::Value>)>,
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

        reporter.on_batch_start(items.len());

        // Enqueue all items
        let mut job_ids = Vec::with_capacity(items.len());
        for (job_type, priority, payload) in items {
            match self.queue.enqueue(&job_type, priority, payload) {
                Ok(id) => job_ids.push(id),
                Err(e) => {
                    tracing::error!(error = %e, "Failed to enqueue job");
                }
            }
        }

        let (result_tx, mut result_rx) = mpsc::channel::<JobResult>(job_ids.len().max(1));
        let mut handles: Vec<JoinHandle<()>> = Vec::new();

        for _ in 0..job_ids.len() {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore not closed");
            let queue = self.queue.clone();
            let cancelled = self.cancelled.clone();
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

                if cancelled.load(Ordering::SeqCst) {
                    return;
                }

                // Claim from queue (blocking storage call)
                let queue_claim = queue.clone();
                let wid = worker_id.clone();
                let job = match tokio::task::spawn_blocking(move || queue_claim.claim(&wid)).await {
                    Ok(Ok(Some(job))) => job,
                    Ok(Ok(None)) => return,
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "Failed to claim job");
                        return;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Task join error");
                        return;
                    }
                };

                // Re-check cancellation after claiming (closes race with ErrorStrategy::Fail)
                if cancelled.load(Ordering::SeqCst) {
                    let q = queue.clone();
                    let jid = job.id;
                    if let Err(e) =
                        tokio::task::spawn_blocking(move || q.fail(&jid, "cancelled".to_string()))
                            .await
                    {
                        tracing::error!(error = %e, "failed to mark job as cancelled");
                    }
                    return;
                }

                let job_id = job.id;
                reporter.on_job_start(&job);

                match processor(job).await {
                    Ok(output) => {
                        let q = queue.clone();
                        if let Err(e) =
                            tokio::task::spawn_blocking(move || q.complete(&job_id, output)).await
                        {
                            tracing::error!(job_id = %job_id, error = %e, "failed to mark job as complete");
                        }
                        completed.fetch_add(1, Ordering::SeqCst);
                        reporter.on_job_complete(job_id, true, None);
                    }
                    Err(error) => {
                        let q = queue.clone();
                        let err = error.clone();
                        if let Err(e) =
                            tokio::task::spawn_blocking(move || q.fail(&job_id, err)).await
                        {
                            tracing::error!(job_id = %job_id, error = %e, "failed to mark job as failed");
                        }
                        failed.fetch_add(1, Ordering::SeqCst);
                        reporter.on_job_complete(job_id, false, Some(&error));

                        if on_error == ErrorStrategy::Fail {
                            cancelled.store(true, Ordering::SeqCst);
                        }
                    }
                }

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
            });

            handles.push(handle);
        }

        // Drop our sender so the channel closes when all workers finish
        drop(result_tx);

        // Collect results
        let mut results = Vec::new();
        while let Some(result) = result_rx.recv().await {
            results.push(result);
        }

        // Wait for all tasks
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

/// How to handle errors during batch processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStrategy {
    /// Stop all processing on first error.
    Fail,
    /// Skip the failed file and continue with remaining.
    Skip,
    /// Continue processing, collecting all errors.
    Continue,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::NoopReporter;
    use crate::test_helpers::InMemoryStore;
    use std::sync::atomic::AtomicU32;

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
        );

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let items: Vec<_> = (0..5).map(|i| (format!("task-{i}"), 100, None)).collect();

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
        );

        let items: Vec<_> = (0..4)
            .map(|i| (format!("task-{i}"), 100, Some(serde_json::json!({"i": i}))))
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
        );

        let items = vec![
            ("fail".into(), 50, None), // will be claimed first (lower priority)
            ("ok".into(), 100, None),
        ];

        pool.process_batch(
            items,
            |job| async move {
                if job.job_type == "fail" {
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
        let pool = WorkerPool::new(queue, WorkerPoolConfig::default());
        assert!(!pool.is_cancelled());
        pool.cancel();
        assert!(pool.is_cancelled());
    }
}
