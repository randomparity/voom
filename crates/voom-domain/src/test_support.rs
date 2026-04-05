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
use std::sync::Mutex;

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
    if !filters.include_missing && file.status == FileStatus::Missing {
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
/// sub-traits with working file and job methods. Plan/stats/plugin-data
/// methods are stubs.
pub struct InMemoryStore {
    files: Mutex<HashMap<Uuid, MediaFile>>,
    jobs: Mutex<HashMap<Uuid, Job>>,
    snapshots: Mutex<Vec<LibrarySnapshot>>,
    event_log: Mutex<Vec<crate::storage::EventLogRecord>>,
    pub pending_ops: Mutex<Vec<PendingOperation>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(Vec::new()),
            event_log: Mutex::new(Vec::new()),
            pending_ops: Mutex::new(Vec::new()),
        }
    }

    /// Builder: seed the store with a file.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn with_file(self, file: MediaFile) -> Self {
        self.files.lock().unwrap().insert(file.id, file);
        self
    }

    /// Builder: seed the store with a job.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn with_job(self, job: Job) -> Self {
        self.jobs.lock().unwrap().insert(job.id, job);
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
        self.files.lock().unwrap().insert(file.id, file.clone());
        Ok(())
    }

    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(self.files.lock().unwrap().get(id).cloned())
    }

    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .cloned())
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let files = self.files.lock().unwrap();
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
        let files = self.files.lock().unwrap();
        let count = files
            .values()
            .filter(|f| matches_filter(f, filters))
            .count();
        Ok(count as u64)
    }

    fn mark_missing(&self, id: &Uuid) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        if let Some(file) = files.get_mut(id) {
            file.status = FileStatus::Missing;
        }
        Ok(())
    }

    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let mut files = self.files.lock().unwrap();
        if let Some(file) = files.get_mut(id) {
            file.status = FileStatus::Active;
            file.path = new_path.to_path_buf();
        }
        Ok(())
    }

    fn purge_missing(&self, _older_than: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let mut files = self.files.lock().unwrap();
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
        let mut files = self.files.lock().unwrap();
        if let Some(file) = files.get_mut(id) {
            file.expected_hash = Some(hash.to_string());
        }
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
        let mut files = self.files.lock().unwrap();
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
        self.jobs.lock().unwrap().insert(job.id, job.clone());
        Ok(job.id)
    }

    fn job(&self, id: &Uuid) -> Result<Option<Job>> {
        Ok(self.jobs.lock().unwrap().get(id).cloned())
    }

    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
        let mut jobs = self.jobs.lock().unwrap();
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
        let mut jobs = self.jobs.lock().unwrap();

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
        let mut jobs = self.jobs.lock().unwrap();
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
        let jobs = self.jobs.lock().unwrap();
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
        let jobs = self.jobs.lock().unwrap();
        let mut counts: HashMap<JobStatus, u64> = HashMap::new();
        for job in jobs.values() {
            *counts.entry(job.status).or_insert(0) += 1;
        }
        Ok(counts.into_iter().collect())
    }

    fn delete_jobs(&self, status: Option<JobStatus>) -> Result<u64> {
        let mut jobs = self.jobs.lock().unwrap();
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
}

impl FileTransitionStorage for InMemoryStore {
    fn record_transition(&self, _: &FileTransition) -> Result<()> {
        Ok(())
    }

    fn transitions_for_file(&self, _: &Uuid) -> Result<Vec<FileTransition>> {
        Ok(Vec::new())
    }

    fn transitions_by_source(&self, _: TransitionSource) -> Result<Vec<FileTransition>> {
        Ok(Vec::new())
    }

    fn transitions_for_path(&self, _: &Path) -> Result<Vec<FileTransition>> {
        Ok(Vec::new())
    }

    fn savings_by_provenance(
        &self,
        _period: Option<crate::stats::TimePeriod>,
    ) -> Result<crate::stats::SavingsReport> {
        Ok(crate::stats::SavingsReport::default())
    }
}

impl PluginDataStorage for InMemoryStore {
    fn plugin_data(&self, _plugin: &str, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn set_plugin_data(&self, _plugin: &str, _key: &str, _value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn delete_plugin_data(&self, _plugin: &str, _key: &str) -> Result<()> {
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
        let mut log = self.event_log.lock().unwrap();
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
        let log = self.event_log.lock().unwrap();
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
            .take(filters.limit.map(|l| l as usize).unwrap_or(usize::MAX))
            .cloned()
            .collect();
        Ok(results)
    }

    fn prune_event_log(&self, keep_last: u64) -> Result<u64> {
        let mut log = self.event_log.lock().unwrap();
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
        self.snapshots.lock().unwrap().push(snapshot.clone());
        Ok(())
    }

    fn latest_snapshot(&self) -> Result<Option<LibrarySnapshot>> {
        Ok(self.snapshots.lock().unwrap().last().cloned())
    }

    fn list_snapshots(&self, limit: u32) -> Result<Vec<LibrarySnapshot>> {
        let snaps = self.snapshots.lock().unwrap();
        let mut result: Vec<_> = snaps.iter().rev().take(limit as usize).cloned().collect();
        result.reverse();
        Ok(result)
    }

    fn prune_snapshots(&self, keep_last: u32) -> Result<u64> {
        let mut snaps = self.snapshots.lock().unwrap();
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
        let mut ops = self.pending_ops.lock().unwrap();
        ops.retain(|o| o.id != op.id);
        ops.push(op.clone());
        Ok(())
    }

    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()> {
        self.pending_ops
            .lock()
            .unwrap()
            .retain(|o| o.id != *plan_id);
        Ok(())
    }

    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>> {
        let mut ops = self.pending_ops.lock().unwrap().clone();
        ops.sort_by_key(|o| o.started_at);
        Ok(ops)
    }
}
