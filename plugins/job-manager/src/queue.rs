//! Job queue backed by `JobStorage`, with priority ordering and status management.

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;
use voom_domain::errors::Result;
use voom_domain::job::{Job, JobStatus, JobType, JobUpdate};
use voom_domain::storage::{JobFilters, JobStorage};

/// Job queue backed by a storage implementation.
///
/// Provides high-level operations for managing the job lifecycle:
/// enqueue, claim, progress, complete, fail, cancel.
pub struct JobQueue {
    store: Arc<dyn JobStorage>,
}

impl JobQueue {
    pub fn new(store: Arc<dyn JobStorage>) -> Self {
        Self { store }
    }

    /// Enqueue a new job with the given type, priority, and optional payload.
    /// Lower priority numbers are processed first.
    pub fn enqueue(
        &self,
        job_type: JobType,
        priority: i32,
        payload: Option<serde_json::Value>,
    ) -> Result<Uuid> {
        let mut job = Job::new(job_type);
        job.priority = priority;
        job.payload = payload;
        self.store.create_job(&job)
    }

    /// Claim the next pending job for the given worker.
    pub fn claim(&self, worker_id: &str) -> Result<Option<Job>> {
        self.store.claim_next_job(worker_id)
    }

    /// Update job progress (0.0 to 1.0) with an optional message.
    pub fn report_progress(
        &self,
        job_id: &Uuid,
        progress: f64,
        message: Option<&str>,
    ) -> Result<()> {
        let update = JobUpdate {
            progress: Some(progress.clamp(0.0, 1.0)),
            progress_message: Some(message.map(String::from)),
            ..Default::default()
        };
        self.store.update_job(job_id, &update)
    }

    /// Mark a job as completed with optional output data.
    pub fn complete(&self, job_id: &Uuid, output: Option<serde_json::Value>) -> Result<()> {
        let update = JobUpdate {
            status: Some(JobStatus::Completed),
            progress: Some(1.0),
            output: Some(output),
            completed_at: Some(Some(Utc::now())),
            ..Default::default()
        };
        self.store.update_job(job_id, &update)
    }

    /// Mark a job as failed with an error message.
    pub fn fail(&self, job_id: &Uuid, error: String) -> Result<()> {
        let update = JobUpdate {
            status: Some(JobStatus::Failed),
            error: Some(Some(error)),
            completed_at: Some(Some(Utc::now())),
            ..Default::default()
        };
        self.store.update_job(job_id, &update)
    }

    /// Cancel a job. Only pending or running jobs can be cancelled.
    ///
    /// Returns an error if the job does not exist or is already in a
    /// terminal state (Completed, Failed, or Cancelled).
    pub fn cancel(&self, job_id: &Uuid) -> Result<()> {
        let job =
            self.store
                .get_job(job_id)?
                .ok_or_else(|| voom_domain::errors::VoomError::Plugin {
                    plugin: "job-manager".into(),
                    message: format!("job {job_id} not found"),
                })?;

        match job.status {
            JobStatus::Pending | JobStatus::Running => {}
            status => {
                return Err(voom_domain::errors::VoomError::Plugin {
                    plugin: "job-manager".into(),
                    message: format!(
                        "cannot cancel job {job_id}: already in terminal state '{status:?}'"
                    ),
                });
            }
        }

        let update = JobUpdate {
            status: Some(JobStatus::Cancelled),
            completed_at: Some(Some(Utc::now())),
            ..Default::default()
        };
        self.store.update_job(job_id, &update)
    }

    /// Claim a specific job by ID for the given worker.
    ///
    /// Returns the job if it was pending and successfully claimed, or None if
    /// the job was not found or not in pending state.
    pub fn claim_by_id(&self, job_id: &Uuid, worker_id: &str) -> Result<Option<Job>> {
        self.store.claim_job_by_id(job_id, worker_id)
    }

    pub fn job(&self, job_id: &Uuid) -> Result<Option<Job>> {
        self.store.get_job(job_id)
    }

    /// List jobs filtered by the given [`JobFilters`].
    pub fn list_jobs(&self, filters: &JobFilters) -> Result<Vec<Job>> {
        self.store.list_jobs(filters)
    }

    /// Get job counts grouped by status.
    pub fn job_counts(&self) -> Result<Vec<(JobStatus, u64)>> {
        self.store.count_jobs_by_status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::test_support::InMemoryStore;

    #[test]
    fn test_enqueue_and_claim() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Transcode, 100, None).unwrap();
        let claimed = queue.claim("worker-1").unwrap().unwrap();
        assert_eq!(claimed.id, id);
        assert_eq!(claimed.status, JobStatus::Running);
    }

    #[test]
    fn test_priority_ordering() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let _low = queue
            .enqueue(JobType::Custom("task-low".into()), 200, None)
            .unwrap();
        let high = queue
            .enqueue(JobType::Custom("task-high".into()), 50, None)
            .unwrap();

        let claimed = queue.claim("w-1").unwrap().unwrap();
        assert_eq!(claimed.id, high);
    }

    #[test]
    fn test_progress_reporting() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Scan, 100, None).unwrap();
        queue.claim("w-1").unwrap();
        queue.report_progress(&id, 0.5, Some("Halfway")).unwrap();

        let job = queue.job(&id).unwrap().unwrap();
        assert_eq!(job.progress, 0.5);
        assert_eq!(job.progress_message.as_deref(), Some("Halfway"));
    }

    #[test]
    fn test_complete_job() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Process, 100, None).unwrap();
        queue.claim("w-1").unwrap();
        queue
            .complete(&id, Some(serde_json::json!({"files": 10})))
            .unwrap();

        let job = queue.job(&id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.progress, 1.0);
        assert!(job.completed_at.is_some());
    }

    #[test]
    fn test_fail_job() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Process, 100, None).unwrap();
        queue.claim("w-1").unwrap();
        queue.fail(&id, "ffmpeg crashed".into()).unwrap();

        let job = queue.job(&id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Failed);
        assert_eq!(job.error.as_deref(), Some("ffmpeg crashed"));
    }

    #[test]
    fn test_cancel_job() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Process, 100, None).unwrap();
        queue.cancel(&id).unwrap();

        let job = queue.job(&id).unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[test]
    fn test_list_jobs() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        queue
            .enqueue(JobType::Custom("a".into()), 100, None)
            .unwrap();
        queue
            .enqueue(JobType::Custom("b".into()), 100, None)
            .unwrap();
        queue
            .enqueue(JobType::Custom("c".into()), 100, None)
            .unwrap();
        queue.claim("w-1").unwrap(); // claims first by priority/time

        let all = queue.list_jobs(&JobFilters::default()).unwrap();
        assert_eq!(all.len(), 3);

        let pending = queue
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Pending),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pending.len(), 2);

        let running = queue
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Running),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(running.len(), 1);
    }

    #[test]
    fn test_counts() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        queue
            .enqueue(JobType::Custom("a".into()), 100, None)
            .unwrap();
        queue
            .enqueue(JobType::Custom("b".into()), 100, None)
            .unwrap();
        queue
            .enqueue(JobType::Custom("c".into()), 100, None)
            .unwrap();
        queue.claim("w-1").unwrap();

        let counts = queue.job_counts().unwrap();
        let pending_count = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Pending)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let running_count = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Running)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(pending_count, 2);
        assert_eq!(running_count, 1);
    }

    #[test]
    fn test_progress_clamping() {
        let store = Arc::new(InMemoryStore::new());
        let queue = JobQueue::new(store);

        let id = queue.enqueue(JobType::Scan, 100, None).unwrap();
        queue.claim("w-1").unwrap();
        queue.report_progress(&id, 1.5, None).unwrap();

        let job = queue.job(&id).unwrap().unwrap();
        assert_eq!(job.progress, 1.0);
    }
}
