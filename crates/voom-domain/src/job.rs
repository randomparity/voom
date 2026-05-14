use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Known job type categories.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobType {
    Process,
    Transcode,
    Scan,
    Introspect,
    /// Extensibility variant for WASM plugins or future job types.
    Custom(String),
}

impl Serialize for JobType {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for JobType {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(JobType::parse(&s))
    }
}

impl JobType {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            JobType::Process => "process",
            JobType::Transcode => "transcode",
            JobType::Scan => "scan",
            JobType::Introspect => "introspect",
            JobType::Custom(s) => s,
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "process" => JobType::Process,
            "transcode" => JobType::Transcode,
            "scan" => JobType::Scan,
            "introspect" => JobType::Introspect,
            other => JobType::Custom(other.to_string()),
        }
    }
}

impl std::fmt::Display for JobType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A background job tracked by the system.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub job_type: JobType,
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
    #[must_use]
    pub fn new(job_type: JobType) -> Self {
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

    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

/// Status of a job.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    #[must_use]
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
#[non_exhaustive]
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
        let job = Job::new(JobType::Transcode);
        assert_eq!(job.job_type, JobType::Transcode);
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.priority, 100);
        assert!(!job.is_terminal());
    }

    #[test]
    fn test_job_is_terminal() {
        let mut job = Job::new(JobType::Scan);
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
        let job = Job::new(JobType::Introspect);
        let json = serde_json::to_string(&job).unwrap();
        let deserialized: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.job_type, JobType::Introspect);
        assert_eq!(deserialized.status, JobStatus::Pending);
    }
}

/// Shared payload for jobs keyed on a discovered file (introspection, processing).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveredFilePayload {
    pub path: String,
    pub size: u64,
    pub content_hash: Option<String>,
    /// Set to `true` when the upstream pipeline determined that this file's
    /// stored row is stale (e.g. an `IngestDecision::Moved` or
    /// `ExternallyChanged` decision was returned by `ingest_discovered_file`)
    /// and the worker must NOT take the stored-row cache hit. Defaults to
    /// `false` so existing call sites (and on-disk snapshots from before
    /// this field was added) get the cache-hit behavior they had before.
    #[serde(default)]
    pub needs_reintrospect: bool,
}
