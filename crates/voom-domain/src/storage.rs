use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bad_file::{BadFile, BadFileSource};
use crate::errors::Result;
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::{Container, MediaFile};
use crate::plan::Plan;
use crate::stats::ProcessingStats;

/// Filters for querying jobs from storage.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct JobFilters {
    pub status: Option<JobStatus>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Filters for querying bad files from storage.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct BadFileFilters {
    pub path_prefix: Option<String>,
    pub error_source: Option<BadFileSource>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Filters for querying files from storage.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct FileFilters {
    pub container: Option<Container>,
    pub has_codec: Option<String>,
    pub has_language: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

// --- Focused sub-traits ---

/// File CRUD operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait FileStorage: Send + Sync {
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
    /// Count total files matching the given filters (ignoring limit/offset).
    fn count_files(&self, filters: &FileFilters) -> Result<u64>;
    fn delete_file(&self, id: &Uuid) -> Result<()>;
}

/// Job queue operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait JobStorage: Send + Sync {
    fn create_job(&self, job: &Job) -> Result<Uuid>;
    fn job(&self, id: &Uuid) -> Result<Option<Job>>;
    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()>;
    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>>;
    /// Atomically claim a specific job by ID, only if it is still pending.
    /// Returns the claimed job (now Running) or None if not pending/not found.
    fn claim_job_by_id(&self, job_id: &Uuid, worker_id: &str) -> Result<Option<Job>>;
    fn list_jobs(&self, filters: &JobFilters) -> Result<Vec<Job>>;
    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>>;
}

/// Plan persistence operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait PlanStorage: Send + Sync {
    fn save_plan(&self, plan: &Plan) -> Result<Uuid>;
    fn plans_for_file(&self, file_id: &Uuid) -> Result<Vec<StoredPlan>>;
    fn update_plan_status(&self, plan_id: &Uuid, status: PlanStatus) -> Result<()>;
}

/// File history snapshots.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait FileHistoryStorage: Send + Sync {
    fn file_history(&self, path: &Path) -> Result<Vec<FileHistoryEntry>>;
}

/// Processing statistics recording.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait StatsStorage: Send + Sync {
    fn record_stats(&self, stats: &ProcessingStats) -> Result<()>;
}

/// Plugin key-value data storage.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait PluginDataStorage: Send + Sync {
    fn plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()>;
    fn delete_plugin_data(&self, plugin: &str, key: &str) -> Result<()>;
}

/// Bad file tracking operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait BadFileStorage: Send + Sync {
    fn upsert_bad_file(&self, bad_file: &BadFile) -> Result<()>;
    fn bad_file_by_path(&self, path: &Path) -> Result<Option<BadFile>>;
    fn list_bad_files(&self, filters: &BadFileFilters) -> Result<Vec<BadFile>>;
    fn count_bad_files(&self, filters: &BadFileFilters) -> Result<u64>;
    fn delete_bad_file(&self, id: &Uuid) -> Result<()>;
    fn delete_bad_file_by_path(&self, path: &Path) -> Result<()>;
}

/// Database maintenance operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
#[allow(clippy::missing_errors_doc)]
pub trait MaintenanceStorage: Send + Sync {
    fn vacuum(&self) -> Result<()>;
    fn prune_missing_files(&self) -> Result<u64>;
    fn prune_missing_files_under(&self, root: &Path) -> Result<u64>;
}

/// Composed storage interface encompassing all sub-traits.
///
/// All methods are synchronous (blocking) since rusqlite is synchronous.
/// Callers should use `tokio::task::spawn_blocking` for async contexts.
///
/// # Errors
///
/// All methods return [`VoomError::Storage`](crate::errors::VoomError::Storage) on database or I/O failures.
pub trait StorageTrait:
    FileStorage
    + JobStorage
    + PlanStorage
    + FileHistoryStorage
    + StatsStorage
    + PluginDataStorage
    + BadFileStorage
    + MaintenanceStorage
{
}

/// Blanket impl: any type implementing all sub-traits automatically implements `StorageTrait`.
impl<T> StorageTrait for T where
    T: FileStorage
        + JobStorage
        + PlanStorage
        + FileHistoryStorage
        + StatsStorage
        + PluginDataStorage
        + BadFileStorage
        + MaintenanceStorage
{
}

/// Status of a stored plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    Executing,
    Completed,
    Failed,
    Skipped,
}

impl PlanStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            PlanStatus::Pending => "pending",
            PlanStatus::Executing => "executing",
            PlanStatus::Completed => "completed",
            PlanStatus::Failed => "failed",
            PlanStatus::Skipped => "skipped",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(PlanStatus::Pending),
            "executing" => Some(PlanStatus::Executing),
            "completed" => Some(PlanStatus::Completed),
            "failed" => Some(PlanStatus::Failed),
            "skipped" => Some(PlanStatus::Skipped),
            _ => None,
        }
    }
}

impl std::fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plan as stored in the database, with its own ID and status tracking.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct StoredPlan {
    pub id: Uuid,
    pub file_id: Uuid,
    pub policy_name: String,
    pub phase_name: String,
    pub status: PlanStatus,
    pub actions_json: String,
    pub warnings: Option<String>,
    pub skip_reason: Option<String>,
    pub policy_hash: Option<String>,
    pub evaluated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub executed_at: Option<DateTime<Utc>>,
    pub result: Option<String>,
}

impl StoredPlan {
    /// Create a new `StoredPlan` with the given identifiers and status.
    #[must_use]
    pub fn new(
        id: Uuid,
        file_id: Uuid,
        policy_name: impl Into<String>,
        phase_name: impl Into<String>,
        status: PlanStatus,
        actions_json: impl Into<String>,
    ) -> Self {
        Self {
            id,
            file_id,
            policy_name: policy_name.into(),
            phase_name: phase_name.into(),
            status,
            actions_json: actions_json.into(),
            warnings: None,
            skip_reason: None,
            policy_hash: None,
            evaluated_at: None,
            created_at: Utc::now(),
            executed_at: None,
            result: None,
        }
    }
}

/// A historical snapshot of a file's state before it was updated.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct FileHistoryEntry {
    pub id: Uuid,
    pub file_id: Uuid,
    pub path: PathBuf,
    pub content_hash: String,
    pub container: Container,
    pub track_count: u32,
    pub introspected_at: DateTime<Utc>,
    pub archived_at: DateTime<Utc>,
}

impl FileHistoryEntry {
    /// Create a new history entry by snapshotting a file's current state.
    #[must_use]
    pub fn from_file(file: &MediaFile, archived_at: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::new_v4(),
            file_id: file.id,
            path: file.path.clone(),
            content_hash: file.content_hash.clone(),
            container: file.container,
            track_count: u32::try_from(file.tracks.len()).unwrap_or(u32::MAX),
            introspected_at: file.introspected_at,
            archived_at,
        }
    }

    /// Reconstruct a history entry from stored fields (e.g., database rows).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_stored(
        id: Uuid,
        file_id: Uuid,
        path: PathBuf,
        content_hash: String,
        container: Container,
        track_count: u32,
        introspected_at: DateTime<Utc>,
        archived_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            file_id,
            path,
            content_hash,
            container,
            track_count,
            introspected_at,
            archived_at,
        }
    }
}
