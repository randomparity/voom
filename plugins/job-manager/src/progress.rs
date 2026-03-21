//! Progress reporting for job processing.
//!
//! Supports two modes:
//! - CLI mode: indicatif progress bars on stderr
//! - Database mode: progress stored via `StorageTrait` for daemon/web UI polling

use uuid::Uuid;
use voom_domain::job::Job;

/// Trait for receiving progress updates from the worker pool.
pub trait ProgressReporter: Send + Sync {
    /// Called when a batch of jobs starts processing.
    fn on_batch_start(&self, total_jobs: usize);

    /// Called when a job starts processing.
    fn on_job_start(&self, job: &Job);

    /// Called when a job's progress changes.
    fn on_job_progress(&self, job_id: Uuid, progress: f64, message: Option<&str>);

    /// Called when a job completes (success or failure).
    fn on_job_complete(&self, job_id: Uuid, success: bool, error: Option<&str>);

    /// Called when all jobs in a batch are done.
    fn on_batch_complete(&self, completed: u64, failed: u64);
}

/// No-op reporter for testing and quiet mode.
pub struct NoopReporter;

impl ProgressReporter for NoopReporter {
    fn on_batch_start(&self, _total: usize) {}
    fn on_job_start(&self, _job: &Job) {}
    fn on_job_progress(&self, _id: Uuid, _progress: f64, _msg: Option<&str>) {}
    fn on_job_complete(&self, _id: Uuid, _success: bool, _error: Option<&str>) {}
    fn on_batch_complete(&self, _completed: u64, _failed: u64) {}
}

/// Reporter that logs progress via tracing.
pub struct TracingReporter;

impl ProgressReporter for TracingReporter {
    fn on_batch_start(&self, total: usize) {
        tracing::info!(total, "Batch processing started");
    }

    fn on_job_start(&self, job: &Job) {
        tracing::info!(
            job_id = %job.id,
            job_type = %job.job_type,
            "Job started"
        );
    }

    fn on_job_progress(&self, job_id: Uuid, progress: f64, message: Option<&str>) {
        tracing::debug!(
            %job_id,
            progress = format!("{:.1}%", progress * 100.0),
            message = message.unwrap_or(""),
            "Job progress"
        );
    }

    fn on_job_complete(&self, job_id: Uuid, success: bool, error: Option<&str>) {
        if success {
            tracing::info!(%job_id, "Job completed");
        } else {
            tracing::warn!(%job_id, error = error.unwrap_or("unknown"), "Job failed");
        }
    }

    fn on_batch_complete(&self, completed: u64, failed: u64) {
        tracing::info!(completed, failed, "Batch processing finished");
    }
}

/// Reporter that stores progress in the database via `StorageTrait`.
///
/// Used in daemon mode where the web UI polls for progress updates.
pub struct StorageReporter {
    store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
}

impl StorageReporter {
    pub fn new(store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>) -> Self {
        Self { store }
    }
}

impl ProgressReporter for StorageReporter {
    fn on_batch_start(&self, _total: usize) {}

    fn on_job_start(&self, _job: &Job) {}

    fn on_job_progress(&self, job_id: Uuid, progress: f64, message: Option<&str>) {
        let update = voom_domain::job::JobUpdate {
            progress: Some(progress),
            progress_message: Some(message.map(|s| s.to_string())),
            ..Default::default()
        };
        if let Err(e) = self.store.update_job(&job_id, &update) {
            tracing::warn!(%job_id, error = %e, "Failed to update job progress in storage");
        }
    }

    fn on_job_complete(&self, _job_id: Uuid, _success: bool, _error: Option<&str>) {
        // Completion is handled by the queue itself
    }

    fn on_batch_complete(&self, _completed: u64, _failed: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    struct CountingReporter {
        batch_starts: AtomicU32,
        job_starts: AtomicU32,
        job_completes: AtomicU32,
        batch_completes: AtomicU32,
    }

    impl CountingReporter {
        fn new() -> Self {
            Self {
                batch_starts: AtomicU32::new(0),
                job_starts: AtomicU32::new(0),
                job_completes: AtomicU32::new(0),
                batch_completes: AtomicU32::new(0),
            }
        }
    }

    impl ProgressReporter for CountingReporter {
        fn on_batch_start(&self, _total: usize) {
            self.batch_starts.fetch_add(1, Ordering::SeqCst);
        }
        fn on_job_start(&self, _job: &Job) {
            self.job_starts.fetch_add(1, Ordering::SeqCst);
        }
        fn on_job_progress(&self, _id: Uuid, _progress: f64, _msg: Option<&str>) {}
        fn on_job_complete(&self, _id: Uuid, _success: bool, _error: Option<&str>) {
            self.job_completes.fetch_add(1, Ordering::SeqCst);
        }
        fn on_batch_complete(&self, _completed: u64, _failed: u64) {
            self.batch_completes.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_noop_reporter() {
        let r = NoopReporter;
        r.on_batch_start(10);
        r.on_job_start(&voom_domain::job::Job::new("test".into()));
        r.on_job_progress(Uuid::new_v4(), 0.5, Some("halfway"));
        r.on_job_complete(Uuid::new_v4(), true, None);
        r.on_batch_complete(10, 0);
    }

    #[test]
    fn test_counting_reporter() {
        let r = CountingReporter::new();
        r.on_batch_start(3);
        assert_eq!(r.batch_starts.load(Ordering::SeqCst), 1);

        let job = voom_domain::job::Job::new("test".into());
        r.on_job_start(&job);
        r.on_job_start(&job);
        assert_eq!(r.job_starts.load(Ordering::SeqCst), 2);

        r.on_job_complete(Uuid::new_v4(), true, None);
        r.on_job_complete(Uuid::new_v4(), false, Some("err"));
        assert_eq!(r.job_completes.load(Ordering::SeqCst), 2);

        r.on_batch_complete(1, 1);
        assert_eq!(r.batch_completes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_storage_reporter() {
        let store = Arc::new(crate::test_support::InMemoryStore::new());
        let reporter = StorageReporter::new(store.clone());

        // Create a job first
        let job = voom_domain::job::Job::new("test".into());
        let job_id = store.create_job(&job).unwrap();

        reporter.on_job_progress(job_id, 0.75, Some("Processing"));

        use voom_domain::storage::StorageTrait;
        let loaded = store.get_job(&job_id).unwrap().unwrap();
        assert_eq!(loaded.progress, 0.75);
        assert_eq!(loaded.progress_message.as_deref(), Some("Processing"));
    }
}
