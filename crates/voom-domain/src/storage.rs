use std::path::Path;

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

/// Abstract storage interface. Implemented by storage plugins (e.g., SQLite).
///
/// All methods are synchronous (blocking) since rusqlite is synchronous.
/// Callers should use `tokio::task::spawn_blocking` for async contexts.
pub trait StorageTrait: Send + Sync {
    // Files
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
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

    // Stats
    fn record_stats(&self, stats: &ProcessingStats) -> Result<()>;

    // Plugin data (key-value)
    fn get_plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()>;

    // Maintenance
    fn vacuum(&self) -> Result<()>;
    fn prune_missing_files(&self) -> Result<u64>;
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
    pub created_at: String,
    pub executed_at: Option<String>,
    pub result: Option<String>,
}
