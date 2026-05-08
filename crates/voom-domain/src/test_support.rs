//! Shared in-memory `StorageTrait` implementation for testing.
//!
//! Gated behind the `testing` feature. Enable in your crate's
//! `[dev-dependencies]` with:
//!
//! ```toml
//! voom-domain = { path = "...", features = ["testing"] }
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use uuid::Uuid;

use crate::bad_file::BadFile;
use crate::errors::{Result, StorageErrorKind, VoomError};
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::{Container, MediaFile, Track, TrackType};
use crate::plan::Plan;
use crate::stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, ProcessingAggregateStats,
    SnapshotTrigger, SubtitleStats, VideoStats,
};
use crate::storage::{
    BadFileFilters, BadFileStorage, FileFilters, FileStorage, FileTransitionStorage,
    HealthCheckFilters, HealthCheckRecord, HealthCheckStorage, JobFilters, JobStorage,
    MaintenanceStorage, PageStats, PendingOperation, PendingOpsStorage, PlanStorage, PlanSummary,
    PluginDataStorage, SnapshotStorage,
};
use crate::transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
use crate::verification::{
    IntegritySummary, VerificationFilters, VerificationMode, VerificationOutcome,
    VerificationRecord,
};

/// Create a standard test `MediaFile` with video, two audio, and one subtitle track.
///
/// Useful as a baseline for evaluator, orchestrator, and condition tests.
#[must_use]
pub fn test_media_file() -> MediaFile {
    let mut file = MediaFile::new(PathBuf::from("/test/movie.mkv"));
    file.container = Container::Mkv;
    file.tracks = vec![
        {
            let mut t = Track::new(0, TrackType::Video, "hevc".into());
            t.width = Some(1920);
            t.height = Some(1080);
            t
        },
        {
            let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
            t.language = "eng".into();
            t.channels = Some(6);
            t.is_default = true;
            t
        },
        {
            let mut t = Track::new(2, TrackType::AudioAlternate, "aac".into());
            t.language = "jpn".into();
            t.channels = Some(2);
            t
        },
        {
            let mut t = Track::new(3, TrackType::SubtitleMain, "srt".into());
            t.language = "eng".into();
            t
        },
    ];
    file
}

fn matches_filter(file: &MediaFile, filters: &FileFilters) -> bool {
    // Mirrors the SQLite backend: when `include_missing` is false, only
    // Active files are returned (excluding both Missing and Quarantined).
    if !filters.include_missing && file.status != FileStatus::Active {
        return false;
    }
    if let Some(container) = filters.container {
        if file.container != container {
            return false;
        }
    }
    if let Some(ref prefix) = filters.path_prefix {
        if !file.path.to_string_lossy().starts_with(prefix.as_str()) {
            return false;
        }
    }
    if let Some(ref codec) = filters.has_codec {
        if !file.tracks.iter().any(|t| t.codec == *codec) {
            return false;
        }
    }
    if let Some(ref lang) = filters.has_language {
        if !file.tracks.iter().any(|t| t.language == *lang) {
            return false;
        }
    }
    true
}

/// In-memory storage for testing. Implements the full `StorageTrait` via
/// sub-traits with working file, job, transition, and plugin-data methods.
/// Plan/stats methods are stubs.
pub struct InMemoryStore {
    files: Mutex<HashMap<Uuid, MediaFile>>,
    jobs: Mutex<HashMap<Uuid, Job>>,
    snapshots: Mutex<Vec<LibrarySnapshot>>,
    event_log: Mutex<Vec<crate::storage::EventLogRecord>>,
    pub pending_ops: Mutex<Vec<PendingOperation>>,
    pub transitions: Mutex<Vec<FileTransition>>,
    pub verifications: Mutex<Vec<VerificationRecord>>,
    plugin_data: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(Vec::new()),
            event_log: Mutex::new(Vec::new()),
            pending_ops: Mutex::new(Vec::new()),
            transitions: Mutex::new(Vec::new()),
            verifications: Mutex::new(Vec::new()),
            plugin_data: Mutex::new(HashMap::new()),
        }
    }

    /// Builder: seed the store with a file.
    pub fn with_file(self, file: MediaFile) -> Self {
        self.files.lock().insert(file.id, file);
        self
    }

    /// Builder: seed the store with a job.
    pub fn with_job(self, job: Job) -> Self {
        self.jobs.lock().insert(job.id, job);
        self
    }

    /// Builder: seed the store with a transition.
    pub fn with_transition(self, transition: FileTransition) -> Self {
        self.transitions.lock().push(transition);
        self
    }

    /// Builder: seed the store with a verification record.
    pub fn with_verification(self, verification: VerificationRecord) -> Self {
        self.verifications.lock().push(verification);
        self
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FileStorage for InMemoryStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        self.files.lock().insert(file.id, file.clone());
        Ok(())
    }

    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(self.files.lock().get(id).cloned())
    }

    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        Ok(self.files.lock().values().find(|f| f.path == path).cloned())
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let files = self.files.lock();
        let mut result: Vec<MediaFile> = files
            .values()
            .filter(|f| matches_filter(f, filters))
            .cloned()
            .collect();
        result.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(offset) = filters.offset {
            result = result.into_iter().skip(offset as usize).collect();
        }
        if let Some(limit) = filters.limit {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    fn count_files(&self, filters: &FileFilters) -> Result<u64> {
        let files = self.files.lock();
        let count = files
            .values()
            .filter(|f| matches_filter(f, filters))
            .count();
        Ok(count as u64)
    }

    fn mark_missing(&self, id: &Uuid) -> Result<()> {
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(id) {
            file.status = FileStatus::Missing;
        }
        Ok(())
    }

    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(id) {
            file.status = FileStatus::Active;
            file.path = new_path.to_path_buf();
        }
        Ok(())
    }

    fn rename_file_path(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(id) {
            file.path = new_path.to_path_buf();
        }
        Ok(())
    }

    fn purge_missing(&self, _older_than: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let mut files = self.files.lock();
        let before = files.len();
        files.retain(|_, f| f.status != FileStatus::Missing);
        Ok((before - files.len()) as u64)
    }

    fn reconcile_discovered_files(
        &self,
        _discovered: &[DiscoveredFile],
        _scanned_dirs: &[std::path::PathBuf],
    ) -> Result<ReconcileResult> {
        Ok(ReconcileResult::default())
    }

    fn update_expected_hash(&self, id: &Uuid, hash: &str) -> Result<()> {
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(id) {
            file.expected_hash = Some(hash.to_string());
        }
        Ok(())
    }

    fn set_file_status(&self, id: &Uuid, status: FileStatus) -> Result<()> {
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(id) {
            file.status = status;
        }
        Ok(())
    }

    /// In-memory stub. Unlike the SQLite implementation, this does NOT roll
    /// back partial mutations on error — the in-memory mutations have no
    /// fallible path. It also does NOT clear `bad_files` rows at the
    /// post-execution path. Tests that exercise rollback semantics or
    /// `bad_files` cleanup must use `SqliteStore`.
    fn record_post_execution(
        &self,
        new_path: Option<&Path>,
        new_expected_hash: Option<&str>,
        transition: &FileTransition,
    ) -> Result<()> {
        debug_assert!(
            new_expected_hash.is_none_or(|h| !h.is_empty()),
            "record_post_execution: empty hash passed as Some — use None instead"
        );
        let mut files = self.files.lock();
        if let Some(file) = files.get_mut(&transition.file_id) {
            if let Some(path) = new_path {
                file.path = path.to_path_buf();
            }
            if let Some(hash) = new_expected_hash {
                file.expected_hash = Some(hash.to_string());
            }
        }
        drop(files);
        self.transitions.lock().push(transition.clone());
        Ok(())
    }

    fn predecessor_of(&self, _successor_id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(None)
    }

    fn predecessor_id_of(&self, _successor_id: &Uuid) -> Result<Option<Uuid>> {
        Ok(None)
    }

    fn mark_missing_paths(
        &self,
        discovered_paths: &[PathBuf],
        scanned_dirs: &[PathBuf],
    ) -> Result<u32> {
        let discovered_set: std::collections::HashSet<&PathBuf> = discovered_paths.iter().collect();
        let mut files = self.files.lock();
        let mut marked = 0u32;
        for file in files.values_mut() {
            if file.status != FileStatus::Active {
                continue;
            }
            let under_scanned = scanned_dirs.iter().any(|dir| file.path.starts_with(dir));
            if under_scanned && !discovered_set.contains(&file.path) {
                file.status = FileStatus::Missing;
                marked += 1;
            }
        }
        Ok(marked)
    }
}

impl JobStorage for InMemoryStore {
    fn create_job(&self, job: &Job) -> Result<Uuid> {
        self.jobs.lock().insert(job.id, job.clone());
        Ok(job.id)
    }

    fn job(&self, id: &Uuid) -> Result<Option<Job>> {
        Ok(self.jobs.lock().get(id).cloned())
    }

    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
        let mut jobs = self.jobs.lock();
        let job = jobs.get_mut(id).ok_or_else(|| VoomError::Storage {
            kind: StorageErrorKind::NotFound,
            message: format!("job {id} not found"),
        })?;

        if let Some(status) = update.status {
            job.status = status;
        }
        if let Some(progress) = update.progress {
            job.progress = progress;
        }
        if let Some(ref msg) = update.progress_message {
            job.progress_message.clone_from(msg);
        }
        if let Some(ref output) = update.output {
            job.output.clone_from(output);
        }
        if let Some(ref error) = update.error {
            job.error.clone_from(error);
        }
        if let Some(ref worker) = update.worker_id {
            job.worker_id.clone_from(worker);
        }
        if let Some(ref started) = update.started_at {
            job.started_at = *started;
        }
        if let Some(ref completed) = update.completed_at {
            job.completed_at = *completed;
        }

        Ok(())
    }

    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>> {
        let mut jobs = self.jobs.lock();

        let job_id = jobs
            .values()
            .filter(|j| j.status == JobStatus::Pending)
            .min_by_key(|j| (j.priority, j.created_at))
            .map(|j| j.id);

        if let Some(id) = job_id {
            let job = jobs.get_mut(&id).unwrap();
            job.status = JobStatus::Running;
            job.worker_id = Some(worker_id.to_string());
            job.started_at = Some(chrono::Utc::now());
            Ok(Some(job.clone()))
        } else {
            Ok(None)
        }
    }

    fn claim_job_by_id(&self, job_id: &Uuid, worker_id: &str) -> Result<Option<Job>> {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.get_mut(job_id) {
            if job.status == JobStatus::Pending {
                job.status = JobStatus::Running;
                job.worker_id = Some(worker_id.to_string());
                job.started_at = Some(chrono::Utc::now());
                return Ok(Some(job.clone()));
            }
        }
        Ok(None)
    }

    fn list_jobs(&self, filters: &JobFilters) -> Result<Vec<Job>> {
        let jobs = self.jobs.lock();
        let mut result: Vec<Job> = jobs
            .values()
            .filter(|j| filters.status.is_none_or(|s| j.status == s))
            .cloned()
            .collect();
        result.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then(b.created_at.cmp(&a.created_at))
        });
        if let Some(offset) = filters.offset {
            result = result.into_iter().skip(offset as usize).collect();
        }
        if let Some(limit) = filters.limit {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>> {
        let jobs = self.jobs.lock();
        let mut counts: HashMap<JobStatus, u64> = HashMap::new();
        for job in jobs.values() {
            *counts.entry(job.status).or_insert(0) += 1;
        }
        Ok(counts.into_iter().collect())
    }

    fn delete_jobs(&self, status: Option<JobStatus>) -> Result<u64> {
        let mut jobs = self.jobs.lock();
        let before = jobs.len();
        match status {
            Some(s) => jobs.retain(|_, j| j.status != s),
            None => jobs.retain(|_, j| {
                !matches!(
                    j.status,
                    JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
                )
            }),
        }
        Ok((before - jobs.len()) as u64)
    }

    fn prune_old_jobs(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }

    fn count_old_jobs(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }

    fn oldest_job_created_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let jobs = self.jobs.lock();
        Ok(jobs.values().map(|j| j.created_at).min())
    }
}

impl PlanStorage for InMemoryStore {
    fn save_plan(&self, _plan: &Plan) -> Result<Uuid> {
        Ok(Uuid::new_v4())
    }

    fn plans_for_file(&self, _file_id: &Uuid) -> Result<Vec<PlanSummary>> {
        Ok(Vec::new())
    }

    fn update_plan_status(
        &self,
        _plan_id: &Uuid,
        _status: crate::storage::PlanStatus,
    ) -> Result<()> {
        Ok(())
    }

    fn plan_stats_by_phase(&self) -> Result<Vec<crate::storage::PlanPhaseStat>> {
        Ok(Vec::new())
    }

    fn update_plan_error(
        &self,
        _plan_id: &Uuid,
        _error: &str,
        _detail: Option<&crate::plan::ExecutionDetail>,
    ) -> Result<()> {
        Ok(())
    }
}

impl FileTransitionStorage for InMemoryStore {
    fn record_transition(&self, transition: &FileTransition) -> Result<()> {
        self.transitions.lock().push(transition.clone());
        Ok(())
    }

    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>> {
        let ts = self.transitions.lock();
        let mut result: Vec<FileTransition> = ts
            .iter()
            .filter(|t| t.file_id == *file_id)
            .cloned()
            .collect();
        result.sort_by_key(|t| t.created_at);
        Ok(result)
    }

    fn transitions_by_source(&self, _: TransitionSource) -> Result<Vec<FileTransition>> {
        Ok(Vec::new())
    }

    fn transitions_for_path(&self, path: &Path) -> Result<Vec<FileTransition>> {
        let ts = self.transitions.lock();
        let mut result: Vec<FileTransition> = ts
            .iter()
            .filter(|t| t.path == path || t.from_path.as_deref() == Some(path))
            .cloned()
            .collect();
        result.sort_by_key(|t| t.created_at);
        Ok(result)
    }

    fn savings_by_provenance(
        &self,
        _period: Option<crate::stats::TimePeriod>,
    ) -> Result<crate::stats::SavingsReport> {
        Ok(crate::stats::SavingsReport::default())
    }

    fn failed_transitions_for_session(
        &self,
        _session_id: &Uuid,
    ) -> Result<Vec<crate::storage::FailedTransition>> {
        Ok(Vec::new())
    }

    fn latest_failure_session(&self) -> Result<Option<Uuid>> {
        Ok(None)
    }

    fn failure_sessions(&self) -> Result<Vec<crate::storage::SessionSummary>> {
        Ok(Vec::new())
    }

    fn prune_old_file_transitions(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }

    fn count_old_file_transitions(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }
}

impl PluginDataStorage for InMemoryStore {
    fn plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .plugin_data
            .lock()
            .get(&(plugin.to_string(), key.to_string()))
            .cloned())
    }

    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()> {
        self.plugin_data
            .lock()
            .insert((plugin.to_string(), key.to_string()), value.to_vec());
        Ok(())
    }

    fn delete_plugin_data(&self, plugin: &str, key: &str) -> Result<()> {
        self.plugin_data
            .lock()
            .remove(&(plugin.to_string(), key.to_string()));
        Ok(())
    }
}

impl BadFileStorage for InMemoryStore {
    fn upsert_bad_file(&self, _bad_file: &BadFile) -> Result<()> {
        Ok(())
    }

    fn bad_file_by_path(&self, _path: &Path) -> Result<Option<BadFile>> {
        Ok(None)
    }

    fn list_bad_files(&self, _filters: &BadFileFilters) -> Result<Vec<BadFile>> {
        Ok(Vec::new())
    }

    fn count_bad_files(&self, _filters: &BadFileFilters) -> Result<u64> {
        Ok(0)
    }

    fn delete_bad_file(&self, _id: &Uuid) -> Result<()> {
        Ok(())
    }

    fn delete_bad_file_by_path(&self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

impl HealthCheckStorage for InMemoryStore {
    fn insert_health_check(&self, _record: &HealthCheckRecord) -> Result<()> {
        Ok(())
    }

    fn list_health_checks(&self, _filters: &HealthCheckFilters) -> Result<Vec<HealthCheckRecord>> {
        Ok(Vec::new())
    }

    fn latest_health_checks(&self) -> Result<Vec<HealthCheckRecord>> {
        Ok(Vec::new())
    }

    fn prune_health_checks(&self, _before: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        Ok(0)
    }
}

impl crate::storage::EventLogStorage for InMemoryStore {
    fn insert_event_log(&self, record: &crate::storage::EventLogRecord) -> Result<i64> {
        let mut log = self.event_log.lock();
        let rowid = (log.len() as i64) + 1;
        let mut stored = record.clone();
        stored.rowid = rowid;
        log.push(stored);
        Ok(rowid)
    }

    fn list_event_log(
        &self,
        filters: &crate::storage::EventLogFilters,
    ) -> Result<Vec<crate::storage::EventLogRecord>> {
        let log = self.event_log.lock();
        let results = log
            .iter()
            .filter(|r| {
                if let Some(ref et) = filters.event_type {
                    let matches = if let Some(prefix) = et.strip_suffix('*') {
                        r.event_type.starts_with(prefix)
                    } else {
                        r.event_type == *et
                    };
                    if !matches {
                        return false;
                    }
                }
                if let Some(since) = filters.since_rowid {
                    if r.rowid <= since {
                        return false;
                    }
                }
                true
            })
            .take(filters.limit.map_or(usize::MAX, |l| l as usize))
            .cloned()
            .collect();
        Ok(results)
    }

    fn prune_event_log(&self, keep_last: u64) -> Result<u64> {
        let mut log = self.event_log.lock();
        let len = log.len();
        let keep = keep_last as usize;
        if len > keep {
            let removed = len - keep;
            *log = log.split_off(removed);
            Ok(removed as u64)
        } else {
            Ok(0)
        }
    }

    fn prune_old_event_log(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }

    fn count_old_event_log(
        &self,
        _policy: crate::storage::RetentionPolicy,
    ) -> crate::errors::Result<crate::storage::PruneReport> {
        Ok(crate::storage::PruneReport::default())
    }

    fn latest_event_of_type(
        &self,
        _event_type: &str,
    ) -> crate::errors::Result<Option<crate::storage::EventLogRecord>> {
        Ok(None)
    }

    fn oldest_event_at(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let events = self.event_log.lock();
        Ok(events.iter().map(|r| r.created_at).min())
    }
}

impl SnapshotStorage for InMemoryStore {
    fn gather_library_stats(&self, trigger: SnapshotTrigger) -> Result<LibrarySnapshot> {
        Ok(LibrarySnapshot {
            id: Uuid::new_v4(),
            captured_at: chrono::Utc::now(),
            trigger,
            files: FileStats::default(),
            video: VideoStats::default(),
            audio: AudioStats::default(),
            subtitles: SubtitleStats::default(),
            processing: ProcessingAggregateStats::default(),
            jobs: JobAggregateStats::default(),
        })
    }

    fn save_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<()> {
        self.snapshots.lock().push(snapshot.clone());
        Ok(())
    }

    fn latest_snapshot(&self) -> Result<Option<LibrarySnapshot>> {
        Ok(self.snapshots.lock().last().cloned())
    }

    fn list_snapshots(&self, limit: u32) -> Result<Vec<LibrarySnapshot>> {
        let snaps = self.snapshots.lock();
        let mut result: Vec<_> = snaps.iter().rev().take(limit as usize).cloned().collect();
        result.reverse();
        Ok(result)
    }

    fn prune_snapshots(&self, keep_last: u32) -> Result<u64> {
        let mut snaps = self.snapshots.lock();
        let len = snaps.len();
        let keep = keep_last as usize;
        if len > keep {
            let removed = len - keep;
            *snaps = snaps.split_off(removed);
            Ok(removed as u64)
        } else {
            Ok(0)
        }
    }
}

impl MaintenanceStorage for InMemoryStore {
    fn vacuum(&self) -> Result<()> {
        Ok(())
    }

    fn prune_missing_files(&self) -> Result<u64> {
        Ok(0)
    }

    fn prune_missing_files_under(&self, _root: &Path) -> Result<u64> {
        Ok(0)
    }

    fn table_row_counts(&self) -> Result<Vec<(String, u64)>> {
        Ok(vec![])
    }

    fn page_stats(&self) -> Result<PageStats> {
        Ok(PageStats {
            page_size: 4096,
            page_count: 0,
            freelist_count: 0,
        })
    }
}

impl PendingOpsStorage for InMemoryStore {
    fn insert_pending_op(&self, op: &PendingOperation) -> Result<()> {
        let mut ops = self.pending_ops.lock();
        ops.retain(|o| o.id != op.id);
        ops.push(op.clone());
        Ok(())
    }

    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()> {
        self.pending_ops.lock().retain(|o| o.id != *plan_id);
        Ok(())
    }

    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>> {
        let mut ops = self.pending_ops.lock().clone();
        ops.sort_by_key(|o| o.started_at);
        Ok(ops)
    }
}

impl crate::storage::VerificationStorage for InMemoryStore {
    fn insert_verification(&self, record: &VerificationRecord) -> Result<()> {
        self.verifications.lock().push(record.clone());
        Ok(())
    }

    fn list_verifications(&self, filters: &VerificationFilters) -> Result<Vec<VerificationRecord>> {
        let mut records: Vec<VerificationRecord> = self
            .verifications
            .lock()
            .iter()
            .filter(|r| matches_verification_filter(r, filters))
            .cloned()
            .collect();
        records.sort_by(|a, b| {
            b.verified_at
                .cmp(&a.verified_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        if let Some(limit) = filters.limit {
            records.truncate(limit as usize);
        }
        Ok(records)
    }

    fn latest_verification(
        &self,
        file_id: &str,
        mode: VerificationMode,
    ) -> Result<Option<VerificationRecord>> {
        let filters = VerificationFilters {
            file_id: Some(file_id.to_string()),
            mode: Some(mode),
            limit: Some(1),
            ..Default::default()
        };
        Ok(self.list_verifications(&filters)?.into_iter().next())
    }

    fn integrity_summary(&self, since: chrono::DateTime<chrono::Utc>) -> Result<IntegritySummary> {
        let files = self.files.lock();
        let verifications = self.verifications.lock();
        let active_files: Vec<_> = files
            .values()
            .filter(|f| f.status == FileStatus::Active)
            .collect();
        let total_files = active_files.len() as u64;
        let mut never_verified = 0;
        let mut stale = 0;
        let mut with_errors = 0;
        let mut with_warnings = 0;
        let mut hash_mismatches = 0;

        for file in active_files {
            let file_id = file.id.to_string();
            let latest = verifications
                .iter()
                .filter(|r| r.file_id == file_id)
                .max_by(|a, b| {
                    a.verified_at
                        .cmp(&b.verified_at)
                        .then_with(|| a.id.cmp(&b.id))
                });
            match latest {
                Some(record) => {
                    if record.verified_at < since {
                        stale += 1;
                    }
                    if record.outcome == VerificationOutcome::Error {
                        with_errors += 1;
                    }
                    if record.outcome == VerificationOutcome::Warning {
                        with_warnings += 1;
                    }
                    if hash_changed_for_latest(&verifications, &file_id, record) {
                        hash_mismatches += 1;
                    }
                }
                None => never_verified += 1,
            }
        }

        Ok(IntegritySummary::new(
            total_files,
            never_verified,
            stale,
            with_errors,
            with_warnings,
            hash_mismatches,
        ))
    }
}

fn hash_changed_for_latest(
    records: &[VerificationRecord],
    file_id: &str,
    latest: &VerificationRecord,
) -> bool {
    if latest.mode != VerificationMode::Hash {
        return false;
    }
    records
        .iter()
        .filter(|r| r.file_id == file_id && r.mode == VerificationMode::Hash)
        .filter(|r| r.verified_at < latest.verified_at)
        .max_by_key(|r| r.verified_at)
        .is_some_and(|previous| previous.content_hash != latest.content_hash)
}

fn matches_verification_filter(record: &VerificationRecord, filters: &VerificationFilters) -> bool {
    if filters
        .file_id
        .as_ref()
        .is_some_and(|id| record.file_id != *id)
    {
        return false;
    }
    if filters.mode.is_some_and(|mode| record.mode != mode) {
        return false;
    }
    if filters
        .outcome
        .is_some_and(|outcome| record.outcome != outcome)
    {
        return false;
    }
    if filters
        .since
        .as_ref()
        .is_some_and(|since| record.verified_at < *since)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod oldest_at_tests {
    use super::*;
    use crate::job::{Job, JobType};
    use crate::storage::{EventLogRecord, EventLogStorage, JobStorage};

    #[test]
    fn oldest_job_created_at_none_when_empty() {
        let store = InMemoryStore::new();
        assert!(store.oldest_job_created_at().unwrap().is_none());
    }

    #[test]
    fn oldest_job_created_at_returns_min_created_at() {
        let store = InMemoryStore::new();
        let mut j1 = Job::new(JobType::Process);
        j1.created_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let mut j2 = Job::new(JobType::Process);
        j2.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        store.create_job(&j1).unwrap();
        store.create_job(&j2).unwrap();
        assert_eq!(
            store
                .oldest_job_created_at()
                .unwrap()
                .map(|t| t.timestamp_millis()),
            Some(j1.created_at.timestamp_millis())
        );
    }

    #[test]
    fn oldest_event_at_none_when_empty() {
        let store = InMemoryStore::new();
        assert!(store.oldest_event_at().unwrap().is_none());
    }

    #[test]
    fn oldest_event_at_returns_min_created_at() {
        let store = InMemoryStore::new();
        let mut a = EventLogRecord::new(
            uuid::Uuid::new_v4(),
            "file.discovered".into(),
            "{}".into(),
            "older".into(),
        );
        a.created_at = chrono::Utc::now() - chrono::Duration::hours(3);
        let mut b = EventLogRecord::new(
            uuid::Uuid::new_v4(),
            "file.discovered".into(),
            "{}".into(),
            "newer".into(),
        );
        b.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        store.insert_event_log(&a).unwrap();
        store.insert_event_log(&b).unwrap();
        assert_eq!(
            store
                .oldest_event_at()
                .unwrap()
                .map(|t| t.timestamp_millis()),
            Some(a.created_at.timestamp_millis())
        );
    }
}
