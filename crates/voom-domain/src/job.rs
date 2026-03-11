use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A background job tracked by the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub job_type: String,
    pub status: JobStatus,
    pub priority: i32,
    pub payload: Option<serde_json::Value>,
    pub progress: f64,
    pub progress_message: Option<String>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub worker_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn new(job_type: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            job_type,
            status: JobStatus::Pending,
            priority: 100,
            payload: None,
            progress: 0.0,
            progress_message: None,
            output: None,
            error: None,
            worker_id: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

/// Status of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(JobStatus::Pending),
            "running" => Some(JobStatus::Running),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            "cancelled" => Some(JobStatus::Cancelled),
            _ => None,
        }
    }
}

/// Partial update to apply to a job.
#[derive(Debug, Clone, Default)]
pub struct JobUpdate {
    pub status: Option<JobStatus>,
    pub progress: Option<f64>,
    pub progress_message: Option<Option<String>>,
    pub output: Option<Option<serde_json::Value>>,
    pub error: Option<Option<String>>,
    pub worker_id: Option<Option<String>>,
    pub started_at: Option<Option<DateTime<Utc>>>,
    pub completed_at: Option<Option<DateTime<Utc>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_new() {
        let job = Job::new("transcode".into());
        assert_eq!(job.job_type, "transcode");
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.priority, 100);
        assert!(!job.is_terminal());
    }

    #[test]
    fn test_job_is_terminal() {
        let mut job = Job::new("scan".into());
        assert!(!job.is_terminal());
        job.status = JobStatus::Completed;
        assert!(job.is_terminal());
        job.status = JobStatus::Failed;
        assert!(job.is_terminal());
        job.status = JobStatus::Running;
        assert!(!job.is_terminal());
    }

    #[test]
    fn test_job_status_roundtrip() {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
            JobStatus::Cancelled,
        ] {
            assert_eq!(JobStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn test_job_serde_json_roundtrip() {
        let job = Job::new("introspect".into());
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.job_type, "introspect");
        assert_eq!(deserialized.status, JobStatus::Pending);
    }
}
