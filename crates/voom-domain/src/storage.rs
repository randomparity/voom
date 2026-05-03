use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bad_file::{BadFile, BadFileSource};
use crate::errors::Result;
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::{Container, MediaFile, StoredFingerprint};
use crate::plan::Plan;
use crate::stats::{LibrarySnapshot, SavingsReport, SnapshotTrigger, TimePeriod};
use crate::transition::{DiscoveredFile, FileTransition, ReconcileResult, TransitionSource};

/// Row retention policy for time- or count-based pruning.
///
/// A row is deleted if **either** `max_age` is exceeded **or** the row's
/// rank (newest first) exceeds `keep_last`. If both fields are `None`,
/// the policy is disabled and the implementing trait method must be a no-op.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetentionPolicy {
    /// Delete rows older than this. `None` means no age bound.
    pub max_age: Option<chrono::Duration>,
    /// Keep at most this many rows (newest-first). `None` means no count bound.
    pub keep_last: Option<u64>,
}

impl RetentionPolicy {
    /// Returns true when no bounds are configured. Trait implementations must
    /// short-circuit and return `PruneReport { deleted: 0, kept: <count> }`
    /// without executing a `DELETE` when this is true.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self.max_age.is_none() && self.keep_last.is_none()
    }
}

/// Outcome of a single `prune_old_*` call.
#[derive(Debug, Clone, Copy, Default)]
pub struct PruneReport {
    /// Rows deleted by this call.
    pub deleted: u64,
    /// Rows that survived (only counts the rows the policy was *eligible* to delete,
    /// i.e., for jobs this excludes pending/running rows).
    pub kept: u64,
}

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
    /// When `true`, include files with `Missing` status. Default: `false`.
    pub include_missing: bool,
}

// --- Focused sub-traits ---

/// File CRUD operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait FileStorage: Send + Sync {
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    /// Fast fingerprint lookup for discovery's hash short-circuit. Default
    /// impl forwards to [`file_by_path`](Self::file_by_path) — backends that
    /// can serve this from a narrower query (e.g. skipping a `tracks` join)
    /// should override it.
    fn file_fingerprint_by_path(&self, path: &Path) -> Result<Option<StoredFingerprint>> {
        Ok(self.file_by_path(path)?.and_then(|f| {
            f.content_hash.map(|h| StoredFingerprint {
                size: f.size,
                content_hash: h,
                last_seen: f.introspected_at,
            })
        }))
    }
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
    /// Count total files matching the given filters (ignoring limit/offset).
    fn count_files(&self, filters: &FileFilters) -> Result<u64>;
    /// Mark a file as missing (soft-delete). The record is retained for history.
    fn mark_missing(&self, id: &Uuid) -> Result<()>;
    /// Restore a missing file to active status, updating its path.
    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()>;
    /// Update the path (and derived filename) of an existing file row,
    /// identified by `id`. After this call, [`Self::file_by_path`] using
    /// `new_path` must return the same row.
    ///
    /// Leaves `status`, `content_hash`, and `expected_hash` untouched;
    /// callers that also need to refresh content metadata should follow
    /// up with [`Self::upsert_file`].
    fn rename_file_path(&self, id: &Uuid, new_path: &Path) -> Result<()>;
    /// Permanently delete all files with Missing status older than `older_than`.
    /// Returns the number of rows purged.
    fn purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64>;
    /// Reconcile a batch of discovered files against stored state.
    fn reconcile_discovered_files(
        &self,
        discovered: &[DiscoveredFile],
        scanned_dirs: &[PathBuf],
    ) -> Result<ReconcileResult>;
    /// Mark active files under `scanned_dirs` as missing if their path is not
    /// in `discovered_paths`. This is a path-only operation — no hash needed.
    fn mark_missing_paths(
        &self,
        discovered_paths: &[PathBuf],
        scanned_dirs: &[PathBuf],
    ) -> Result<u32>;
    /// Update the expected hash for a file (set after a successful voom operation).
    fn update_expected_hash(&self, id: &Uuid, hash: &str) -> Result<()>;
    /// Find the file that was superseded by the given file (predecessor lookup).
    /// Returns the file whose `superseded_by` field equals `successor_id`.
    fn predecessor_of(&self, successor_id: &Uuid) -> Result<Option<MediaFile>>;
    /// Lightweight predecessor lookup — returns only the UUID, no tracks.
    /// Used by lineage chain walking where only the ID is needed.
    fn predecessor_id_of(&self, successor_id: &Uuid) -> Result<Option<Uuid>>;
}

/// Job queue operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
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
    /// Delete jobs by status. If `status` is `Some`, delete only jobs
    /// with that status. If `None`, delete all terminal jobs
    /// (completed, failed, cancelled). Never deletes pending/running.
    /// Returns the number of deleted rows.
    fn delete_jobs(&self, status: Option<JobStatus>) -> Result<u64>;
    /// Delete terminal-state jobs (`completed` / `failed` / `cancelled`) per `policy`.
    ///
    /// Pending and running jobs are never touched, regardless of age. Returns
    /// the number deleted and the number that survived (eligible only).
    fn prune_old_jobs(&self, policy: RetentionPolicy) -> Result<PruneReport>;
}

/// Plan persistence operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait PlanStorage: Send + Sync {
    fn save_plan(&self, plan: &Plan) -> Result<Uuid>;
    fn plans_for_file(&self, file_id: &Uuid) -> Result<Vec<PlanSummary>>;
    fn update_plan_status(&self, plan_id: &Uuid, status: PlanStatus) -> Result<()>;
    /// Aggregate plan counts grouped by phase name, status, and skip reason.
    fn plan_stats_by_phase(&self) -> Result<Vec<PlanPhaseStat>>;
    /// Write error info and optional execution detail to the plan's `result` column.
    fn update_plan_error(
        &self,
        plan_id: &Uuid,
        error: &str,
        detail: Option<&crate::plan::ExecutionDetail>,
    ) -> Result<()>;
}

/// File lifecycle transition recording.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait FileTransitionStorage: Send + Sync {
    /// Record a single file transition.
    fn record_transition(&self, transition: &FileTransition) -> Result<()>;
    /// Retrieve all transitions for a specific file, ordered by `created_at`.
    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>>;
    /// Retrieve all transitions with the given source, ordered by `created_at`.
    fn transitions_by_source(&self, source: TransitionSource) -> Result<Vec<FileTransition>>;
    /// Retrieve all transitions recorded at a specific path, across all file IDs.
    /// Ordered by `created_at`. This includes transitions from superseded file
    /// records that were replaced at this path.
    fn transitions_for_path(&self, path: &Path) -> Result<Vec<FileTransition>>;
    /// Aggregate space savings from successful voom transitions, grouped by
    /// executor (`source_detail`), phase (`phase_name`), and optionally by
    /// time period.
    fn savings_by_provenance(&self, period: Option<TimePeriod>) -> Result<SavingsReport>;
    /// Retrieve failed transitions for a specific session.
    fn failed_transitions_for_session(&self, session_id: &Uuid) -> Result<Vec<FailedTransition>>;
    /// Find the most recent session that has failures.
    fn latest_failure_session(&self) -> Result<Option<Uuid>>;
    /// List sessions that have failures, most recent first.
    fn failure_sessions(&self) -> Result<Vec<SessionSummary>>;
}

/// A failed transition with plan result details for error reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedTransition {
    pub path: PathBuf,
    pub phase_name: Option<String>,
    pub error_message: Option<String>,
    pub session_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    /// JSON from plans.result containing `ExecutionDetail`.
    pub plan_result: Option<String>,
}

/// Summary of a processing session with failure counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub failure_count: u64,
}

/// Plugin key-value data storage.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait PluginDataStorage: Send + Sync {
    fn plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()>;
    fn delete_plugin_data(&self, plugin: &str, key: &str) -> Result<()>;
}

/// Bad file tracking operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait BadFileStorage: Send + Sync {
    fn upsert_bad_file(&self, bad_file: &BadFile) -> Result<()>;
    fn bad_file_by_path(&self, path: &Path) -> Result<Option<BadFile>>;
    fn list_bad_files(&self, filters: &BadFileFilters) -> Result<Vec<BadFile>>;
    fn count_bad_files(&self, filters: &BadFileFilters) -> Result<u64>;
    fn delete_bad_file(&self, id: &Uuid) -> Result<()>;
    fn delete_bad_file_by_path(&self, path: &Path) -> Result<()>;
}

/// A single health check result with timestamp.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheckRecord {
    pub id: Uuid,
    pub check_name: String,
    pub passed: bool,
    pub details: Option<String>,
    pub checked_at: DateTime<Utc>,
}

impl HealthCheckRecord {
    #[must_use]
    pub fn new(check_name: impl Into<String>, passed: bool, details: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            check_name: check_name.into(),
            passed,
            details,
            checked_at: Utc::now(),
        }
    }

    /// Reconstruct a record from stored fields (e.g., database rows).
    #[must_use]
    pub fn from_stored(
        id: Uuid,
        check_name: String,
        passed: bool,
        details: Option<String>,
        checked_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            check_name,
            passed,
            details,
            checked_at,
        }
    }
}

/// Filters for querying health check history.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct HealthCheckFilters {
    pub check_name: Option<String>,
    pub passed: Option<bool>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<u32>,
}

/// Health check history storage operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait HealthCheckStorage: Send + Sync {
    fn insert_health_check(&self, record: &HealthCheckRecord) -> Result<()>;
    fn list_health_checks(&self, filters: &HealthCheckFilters) -> Result<Vec<HealthCheckRecord>>;
    /// Latest result per `check_name` (for the `/api/health` summary).
    fn latest_health_checks(&self) -> Result<Vec<HealthCheckRecord>>;
    /// Delete records older than `before`. Returns the number of rows deleted.
    fn prune_health_checks(&self, before: DateTime<Utc>) -> Result<u64>;
}

/// A single event log entry.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLogRecord {
    pub rowid: i64,
    pub id: Uuid,
    pub event_type: String,
    pub payload: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

impl EventLogRecord {
    #[must_use]
    pub fn new(id: Uuid, event_type: String, payload: String, summary: String) -> Self {
        Self {
            rowid: 0,
            id,
            event_type,
            payload,
            summary,
            created_at: Utc::now(),
        }
    }

    /// Reconstruct a record from stored fields.
    #[must_use]
    pub fn from_stored(
        rowid: i64,
        id: Uuid,
        event_type: String,
        payload: String,
        summary: String,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            rowid,
            id,
            event_type,
            payload,
            summary,
            created_at,
        }
    }
}

/// Filters for querying the event log.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct EventLogFilters {
    pub event_type: Option<String>,
    pub since_rowid: Option<i64>,
    pub limit: Option<u32>,
}

/// Event log storage operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait EventLogStorage: Send + Sync {
    fn insert_event_log(&self, record: &EventLogRecord) -> Result<i64>;
    fn list_event_log(&self, filters: &EventLogFilters) -> Result<Vec<EventLogRecord>>;
    fn prune_event_log(&self, keep_last: u64) -> Result<u64>;
    /// Delete event_log rows per `policy`.
    ///
    /// Age is measured by `created_at`. Rank is by `rowid` DESC (newest first).
    fn prune_old_event_log(&self, policy: RetentionPolicy) -> Result<PruneReport>;
}

/// Library snapshot storage operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait SnapshotStorage: Send + Sync {
    /// Gather live library statistics from the database.
    fn gather_library_stats(&self, trigger: SnapshotTrigger) -> Result<LibrarySnapshot>;
    /// Persist a snapshot for history tracking.
    fn save_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<()>;
    /// Retrieve the most recent snapshot.
    fn latest_snapshot(&self) -> Result<Option<LibrarySnapshot>>;
    /// List snapshots ordered by `captured_at` descending.
    fn list_snapshots(&self, limit: u32) -> Result<Vec<LibrarySnapshot>>;
    /// Delete all but the newest `keep_last` snapshots. Returns rows deleted.
    fn prune_snapshots(&self, keep_last: u32) -> Result<u64>;
}

/// `SQLite` page-level statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PageStats {
    pub page_size: u64,
    pub page_count: u64,
    pub freelist_count: u64,
}

/// Database maintenance operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait MaintenanceStorage: Send + Sync {
    fn vacuum(&self) -> Result<()>;
    fn prune_missing_files(&self) -> Result<u64>;
    fn prune_missing_files_under(&self, root: &Path) -> Result<u64>;
    fn table_row_counts(&self) -> Result<Vec<(String, u64)>>;
    fn page_stats(&self) -> Result<PageStats>;
}

/// A record of an in-flight plan execution, used for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOperation {
    pub id: Uuid,
    pub file_path: PathBuf,
    pub phase_name: String,
    pub started_at: DateTime<Utc>,
}

/// Pending operation tracking for crash recovery.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait PendingOpsStorage: Send + Sync {
    fn insert_pending_op(&self, op: &PendingOperation) -> Result<()>;
    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()>;
    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>>;
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
    + FileTransitionStorage
    + PluginDataStorage
    + BadFileStorage
    + MaintenanceStorage
    + HealthCheckStorage
    + EventLogStorage
    + SnapshotStorage
    + PendingOpsStorage
{
}

/// Blanket impl: any type implementing all sub-traits automatically implements `StorageTrait`.
impl<T> StorageTrait for T where
    T: FileStorage
        + JobStorage
        + PlanStorage
        + FileTransitionStorage
        + PluginDataStorage
        + BadFileStorage
        + MaintenanceStorage
        + HealthCheckStorage
        + EventLogStorage
        + SnapshotStorage
        + PendingOpsStorage
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

/// Aggregated plan statistics for a single (`phase_name`, status, `skip_reason`) group.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PlanPhaseStat {
    pub phase_name: String,
    pub status: PlanStatus,
    pub skip_reason: Option<String>,
    pub count: u64,
}

impl PlanPhaseStat {
    #[must_use]
    pub fn new(
        phase_name: String,
        status: PlanStatus,
        skip_reason: Option<String>,
        count: u64,
    ) -> Self {
        Self {
            phase_name,
            status,
            skip_reason,
            count,
        }
    }
}

/// A plan summary with typed actions, suitable for API responses and templates.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub id: Uuid,
    pub file_id: Uuid,
    pub policy_name: String,
    pub phase_name: String,
    pub status: PlanStatus,
    pub actions: Vec<crate::plan::PlannedAction>,
    pub warnings: Vec<String>,
    pub skip_reason: Option<String>,
    pub policy_hash: Option<String>,
    pub evaluated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub executed_at: Option<DateTime<Utc>>,
    pub result: Option<String>,
}

impl PlanSummary {
    #[must_use]
    pub fn new(
        id: Uuid,
        file_id: Uuid,
        policy_name: impl Into<String>,
        phase_name: impl Into<String>,
        status: PlanStatus,
        actions: Vec<crate::plan::PlannedAction>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            file_id,
            policy_name: policy_name.into(),
            phase_name: phase_name.into(),
            status,
            actions,
            warnings: Vec::new(),
            skip_reason: None,
            policy_hash: None,
            evaluated_at: None,
            created_at,
            executed_at: None,
            result: None,
        }
    }
}

#[cfg(test)]
mod retention_policy_tests {
    use super::{PruneReport, RetentionPolicy};

    #[test]
    fn retention_policy_disabled_when_both_none() {
        let p = RetentionPolicy {
            max_age: None,
            keep_last: None,
        };
        assert!(p.is_disabled());
    }

    #[test]
    fn retention_policy_not_disabled_when_age_set() {
        let p = RetentionPolicy {
            max_age: Some(chrono::Duration::days(1)),
            keep_last: None,
        };
        assert!(!p.is_disabled());
    }

    #[test]
    fn retention_policy_not_disabled_when_count_set() {
        let p = RetentionPolicy {
            max_age: None,
            keep_last: Some(10),
        };
        assert!(!p.is_disabled());
    }

    #[test]
    fn prune_report_default_is_zero() {
        let r = PruneReport::default();
        assert_eq!(r.deleted, 0);
        assert_eq!(r.kept, 0);
    }
}
