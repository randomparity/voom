use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::errors::Result;
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::MediaFile;
use crate::plan::Plan;
use crate::stats::ProcessingStats;

/// Filters for querying files from storage.
#[derive(Debug, Clone, Default)]
pub struct FileFilters {
    pub container: Option<String>,
    pub has_codec: Option<String>,
    pub has_language: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Abstract storage interface. Implemented by storage plugins (e.g., `SQLite`).
///
/// All methods are synchronous (blocking) since rusqlite is synchronous.
/// Callers should use `tokio::task::spawn_blocking` for async contexts.
pub trait StorageTrait: Send + Sync {
    // Files
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
    /// Count total files matching the given filters (ignoring limit/offset).
    fn count_files(&self, filters: &FileFilters) -> Result<u64>;
    fn delete_file(&self, id: &Uuid) -> Result<()>;

    // Jobs
    fn create_job(&self, job: &Job) -> Result<Uuid>;
    fn get_job(&self, id: &Uuid) -> Result<Option<Job>>;
    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()>;
    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>>;
    fn list_jobs(&self, status: Option<JobStatus>, limit: Option<u32>) -> Result<Vec<Job>>;
    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>>;

    // Plans
    fn save_plan(&self, plan: &Plan) -> Result<Uuid>;
    fn get_plans_for_file(&self, file_id: &Uuid) -> Result<Vec<StoredPlan>>;
    // TODO: Replace `status: &str` with a typed `PlanStatus` enum for compile-time
    // safety. Deferred because it requires coordinating changes across StorageTrait,
    // SqliteStore, StoredPlan, and all test helpers.
    fn update_plan_status(&self, plan_id: &Uuid, status: &str) -> Result<()>;

    // File history
    fn get_file_history(&self, path: &Path) -> Result<Vec<FileHistoryEntry>>;

    // Stats
    fn record_stats(&self, stats: &ProcessingStats) -> Result<()>;

    // Plugin data (key-value)
    fn get_plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()>;

    // Maintenance
    fn vacuum(&self) -> Result<()>;
    fn prune_missing_files(&self) -> Result<u64>;
    fn prune_missing_files_under(&self, root: &Path) -> Result<u64>;
}

/// A plan as stored in the database, with its own ID and status tracking.
#[derive(Debug, Clone)]
pub struct StoredPlan {
    pub id: Uuid,
    pub file_id: Uuid,
    pub policy_name: String,
    pub phase_name: String,
    pub status: String,
    pub actions_json: String,
    pub warnings: Option<String>,
    pub skip_reason: Option<String>,
    pub policy_hash: Option<String>,
    pub evaluated_at: Option<String>,
    pub created_at: String,
    pub executed_at: Option<String>,
    pub result: Option<String>,
}

/// A historical snapshot of a file's state before it was updated.
#[derive(Debug, Clone)]
pub struct FileHistoryEntry {
    pub id: Uuid,
    pub file_id: Uuid,
    pub path: PathBuf,
    pub content_hash: String,
    pub container: String,
    pub track_count: u32,
    pub introspected_at: String,
    pub archived_at: String,
}
