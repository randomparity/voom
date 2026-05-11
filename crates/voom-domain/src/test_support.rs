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
use crate::estimate::{CostModelSample, EstimateRun};
use crate::job::{Job, JobStatus, JobUpdate};
use crate::media::{Container, MediaFile, Track, TrackType};
use crate::plan::Plan;
use crate::stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, ProcessingAggregateStats,
    SnapshotTrigger, SubtitleStats, VideoStats,
};
use crate::storage::{
    BadFileFilters, BadFileStorage, CostModelSampleFilters, EstimateStorage, FileFilters,
    FileStorage, FileTransitionStorage, HealthCheckFilters, HealthCheckRecord, HealthCheckStorage,
    JobFilters, JobStorage, MaintenanceStorage, PageStats, PendingOperation, PendingOpsStorage,
    PlanStorage, PlanSummary, PluginDataStorage, SnapshotStorage, TranscodeOutcomeFilters,
    TranscodeOutcomeStorage,
};
use crate::transcode::TranscodeOutcome;
use crate::transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
use crate::verification::{
    IntegritySummary, IntegritySummaryInput, VerificationFilters, VerificationMode,
    VerificationOutcome, VerificationRecord,
};

#[derive(Debug, Clone)]
struct InMemorySessionRow {
    roots: Vec<std::path::PathBuf>,
    status: crate::transition::ScanSessionStatus,
    last_heartbeat_at: chrono::DateTime<chrono::Utc>,
}

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
///
// Lock acquisition order (to prevent deadlock):
//   1. self.sessions             (scan session lifecycle)
//   2. self.last_seen            (per-path session stamps)
//   3. self.files                (file rows)
//   4. self.file_session_affinity (file → session that last touched it)
// Always acquire in this order when holding multiple simultaneously.
// `new_in_session` and `transitions` are always acquired in isolation
// (never while holding sessions/last_seen/files).
pub struct InMemoryStore {
    files: Mutex<HashMap<Uuid, MediaFile>>,
    jobs: Mutex<HashMap<Uuid, Job>>,
    snapshots: Mutex<Vec<LibrarySnapshot>>,
    event_log: Mutex<Vec<crate::storage::EventLogRecord>>,
    estimate_runs: Mutex<Vec<EstimateRun>>,
    cost_model_samples: Mutex<Vec<CostModelSample>>,
    pub pending_ops: Mutex<Vec<PendingOperation>>,
    pub transcode_outcomes: Mutex<Vec<TranscodeOutcome>>,
    pub transitions: Mutex<Vec<FileTransition>>,
    pub verifications: Mutex<Vec<VerificationRecord>>,
    plugin_data: Mutex<HashMap<(String, String), Vec<u8>>>,
    sessions: Mutex<HashMap<crate::transition::ScanSessionId, InMemorySessionRow>>,
    last_seen: Mutex<HashMap<std::path::PathBuf, crate::transition::ScanSessionId>>,
    /// Tracks which file IDs were inserted as `New` (not Moved/Unchanged/etc.)
    /// during each open scan session, so finish_scan_session can promote them.
    new_in_session:
        Mutex<HashMap<crate::transition::ScanSessionId, std::collections::HashSet<Uuid>>>,
    /// Maps file_id → ScanSessionId of the session that last touched it.
    /// Equivalent to SqliteStore's `files.last_seen_session_id` column.
    file_session_affinity: Mutex<HashMap<Uuid, crate::transition::ScanSessionId>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(Vec::new()),
            event_log: Mutex::new(Vec::new()),
            estimate_runs: Mutex::new(Vec::new()),
            cost_model_samples: Mutex::new(Vec::new()),
            pending_ops: Mutex::new(Vec::new()),
            transcode_outcomes: Mutex::new(Vec::new()),
            transitions: Mutex::new(Vec::new()),
            verifications: Mutex::new(Vec::new()),
            plugin_data: Mutex::new(HashMap::new()),
            sessions: Default::default(),
            last_seen: Default::default(),
            new_in_session: Default::default(),
            file_session_affinity: Default::default(),
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

    /// Builder: seed the store with a transcode outcome.
    pub fn with_transcode_outcome(self, outcome: TranscodeOutcome) -> Self {
        self.transcode_outcomes.lock().push(outcome);
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
        discovered: &[DiscoveredFile],
        scanned_dirs: &[std::path::PathBuf],
    ) -> Result<ReconcileResult> {
        use crate::transition::IngestDecision;

        let session = self.begin_scan_session(scanned_dirs)?;
        let mut result = ReconcileResult::default();

        let outcome: Result<()> = (|| {
            for df in discovered {
                let decision = self.ingest_discovered_file(session, df)?;
                match &decision {
                    IngestDecision::New { .. } => result.new_files += 1,
                    IngestDecision::Unchanged { .. } => result.unchanged += 1,
                    IngestDecision::ExternallyChanged { .. } => result.external_changes += 1,
                    IngestDecision::Moved { .. } => result.moved += 1,
                    IngestDecision::Duplicate { .. } => {}
                }
                if let Some(p) = decision.needs_introspection_path(&df.path) {
                    result.needs_introspection.push(p);
                }
            }
            let finish = self.finish_scan_session(session)?;
            result.missing = finish.missing;
            result.new_files = result.new_files.saturating_sub(finish.promoted_moves);
            result.moved += finish.promoted_moves;
            Ok(())
        })();

        match outcome {
            Ok(()) => Ok(result),
            Err(e) => {
                let _ = self.cancel_scan_session(session);
                Err(e)
            }
        }
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

    fn begin_scan_session(&self, roots: &[PathBuf]) -> Result<crate::transition::ScanSessionId> {
        const STALE_SESSION_SECS: i64 = 60;
        let now = chrono::Utc::now();

        let mut sessions = self.sessions.lock();

        // Find any LIVE in_progress session — fail fast with that ID.
        for (sid, row) in sessions.iter() {
            if row.status == crate::transition::ScanSessionStatus::InProgress {
                let age_secs = now
                    .signed_duration_since(row.last_heartbeat_at)
                    .num_seconds();
                if age_secs <= STALE_SESSION_SECS {
                    return Err(crate::errors::VoomError::Other(
                        format!(
                            "another scan is in progress (session {sid}, heartbeat \
                             {age_secs} seconds ago); wait up to {STALE_SESSION_SECS} seconds \
                             or abandon it manually if stale"
                        )
                        .into(),
                    ));
                }
            }
        }

        // Auto-cancel stale in_progress sessions.
        for row in sessions.values_mut() {
            if row.status == crate::transition::ScanSessionStatus::InProgress {
                row.status = crate::transition::ScanSessionStatus::Cancelled;
            }
        }

        let id = crate::transition::ScanSessionId::new();
        sessions.insert(
            id,
            InMemorySessionRow {
                roots: roots.to_vec(),
                status: crate::transition::ScanSessionStatus::InProgress,
                last_heartbeat_at: now,
            },
        );
        Ok(id)
    }

    fn ingest_discovered_file(
        &self,
        session: crate::transition::ScanSessionId,
        file: &crate::transition::DiscoveredFile,
    ) -> Result<crate::transition::IngestDecision> {
        // Step 1: Acquire sessions: validate, bump heartbeat, snapshot roots + statuses.
        // Lock is released before acquiring last_seen/files (lock order: 1→2→3→4).
        let (session_roots, session_status_snapshot) = {
            let mut sessions = self.sessions.lock();
            let row = sessions.get_mut(&session).ok_or_else(|| {
                crate::errors::VoomError::Other(format!("unknown scan session {session}").into())
            })?;
            if row.status != crate::transition::ScanSessionStatus::InProgress {
                return Err(crate::errors::VoomError::Other(
                    format!(
                        "scan session {session} is not in_progress (status: {:?})",
                        row.status,
                    )
                    .into(),
                ));
            }
            row.last_heartbeat_at = chrono::Utc::now();
            let roots = row.roots.clone();
            // Snapshot all session statuses so we can do stub-recovery lookups
            // without re-acquiring the sessions lock later.
            let statuses: HashMap<
                crate::transition::ScanSessionId,
                crate::transition::ScanSessionStatus,
            > = sessions.iter().map(|(k, v)| (*k, v.status)).collect();
            (roots, statuses)
        };

        // Step 2: Acquire last_seen, do duplicate fast-path.
        let mut last_seen = self.last_seen.lock();
        if last_seen.get(&file.path) == Some(&session) {
            let files = self.files.lock();
            let id = files
                .values()
                .find(|f| f.path == file.path)
                .map(|f| f.id)
                .ok_or_else(|| {
                    crate::errors::VoomError::Other(
                        format!(
                            "duplicate path {} marked seen but not in files map",
                            file.path.display(),
                        )
                        .into(),
                    )
                })?;
            return Ok(crate::transition::IngestDecision::Duplicate { file_id: id });
        }

        // Step 3: Acquire files, do existing-row branches.
        let mut files = self.files.lock();
        let existing = files.values().find(|f| f.path == file.path).cloned();

        if let Some(mut existing) = existing {
            // Look up affinity (lock order: sessions→last_seen→files→file_session_affinity).
            // We already hold last_seen and files, so we acquire affinity now.
            let affinity_lookup: Option<crate::transition::ScanSessionId> = {
                let affinity = self.file_session_affinity.lock();
                affinity.get(&existing.id).copied()
            };
            // Stub recovery: check whether this row was last touched by a
            // non-completed session. If so, it's a stub from an interrupted scan.
            let needs_recovery = match affinity_lookup {
                Some(prior_session) => !matches!(
                    session_status_snapshot.get(&prior_session),
                    Some(crate::transition::ScanSessionStatus::Completed)
                ),
                None => false,
            };

            let hash_matches = existing
                .expected_hash
                .as_ref()
                .is_none_or(|eh| eh == &file.content_hash);

            if needs_recovery {
                // Stub from a non-completed session — delete unconditionally
                // (regardless of hash match), then fall through to the no-row path.
                let stub_id = existing.id;
                files.remove(&stub_id);
                drop(files);
                drop(last_seen);
                self.file_session_affinity.lock().remove(&stub_id);
                // Re-acquire locks in documented order so the no-row path proceeds normally.
                last_seen = self.last_seen.lock();
                files = self.files.lock();
                // Fall through to no-row path (existing is dropped; files has no entry now).
            } else if hash_matches {
                // Unchanged
                existing.status = FileStatus::Active;
                if existing.expected_hash.is_none() {
                    existing.expected_hash = Some(file.content_hash.clone());
                }
                let id = existing.id;
                files.insert(existing.id, existing);
                last_seen.insert(file.path.clone(), session);
                drop(files);
                drop(last_seen);
                self.file_session_affinity.lock().insert(id, session);
                return Ok(crate::transition::IngestDecision::Unchanged { file_id: id });
            } else {
                // !hash_matches && !needs_recovery: ExternallyChanged
                let old_id = existing.id;
                existing.status = FileStatus::Missing;
                files.insert(old_id, existing);

                let new_id = Uuid::new_v4();
                let mut new_file = MediaFile::new(file.path.clone());
                new_file.id = new_id;
                new_file.size = file.size;
                new_file.content_hash = Some(file.content_hash.clone());
                new_file.expected_hash = Some(file.content_hash.clone());
                new_file.status = FileStatus::Active;
                files.insert(new_id, new_file);
                last_seen.insert(file.path.clone(), session);
                drop(files);
                drop(last_seen);
                self.file_session_affinity.lock().insert(new_id, session);
                return Ok(crate::transition::IngestDecision::ExternallyChanged {
                    file_id: new_id,
                    superseded: old_id,
                });
            }
        }

        // No row at this path (either never existed, or stub was just deleted above).
        // Check for move via missing+expected_hash match.
        // Constrain to session roots to avoid cross-root hash-collision false positives.
        let move_match = files
            .values()
            .find(|f| {
                f.status == FileStatus::Missing
                    && f.expected_hash.as_deref() == Some(file.content_hash.as_str())
                    && session_roots.iter().any(|r| f.path.starts_with(r))
            })
            .cloned();
        if let Some(mut m) = move_match {
            let from_path = m.path.clone();
            m.path = file.path.clone();
            m.status = FileStatus::Active;
            m.size = file.size;
            m.content_hash = Some(file.content_hash.clone());
            let id = m.id;
            files.insert(id, m);
            last_seen.insert(file.path.clone(), session);
            drop(files);
            drop(last_seen);
            self.file_session_affinity.lock().insert(id, session);
            return Ok(crate::transition::IngestDecision::Moved {
                file_id: id,
                from_path,
            });
        }

        // Truly new
        let new_id = Uuid::new_v4();
        let mut new_file = MediaFile::new(file.path.clone());
        new_file.id = new_id;
        new_file.size = file.size;
        new_file.content_hash = Some(file.content_hash.clone());
        new_file.expected_hash = Some(file.content_hash.clone());
        new_file.status = FileStatus::Active;
        files.insert(new_id, new_file);
        last_seen.insert(file.path.clone(), session);
        // Release other locks before acquiring new_in_session and affinity
        // (preserve lock order — these are both isolation-only locks).
        drop(files);
        drop(last_seen);
        // Track this file ID as new in the session for move-promotion at finish time.
        self.new_in_session
            .lock()
            .entry(session)
            .or_default()
            .insert(new_id);
        self.file_session_affinity.lock().insert(new_id, session);
        Ok(crate::transition::IngestDecision::New {
            file_id: new_id,
            needs_introspection: true,
        })
    }

    fn finish_scan_session(
        &self,
        session: crate::transition::ScanSessionId,
    ) -> Result<crate::transition::ScanFinishOutcome> {
        use crate::transition::ScanFinishOutcome;

        // Step 1: validate session status and clone roots, then release sessions
        // before acquiring last_seen and files (preserves documented lock order).
        let roots = {
            let mut sessions = self.sessions.lock();
            let row = sessions.get_mut(&session).ok_or_else(|| {
                crate::errors::VoomError::Other(format!("unknown scan session {session}").into())
            })?;
            if row.status != crate::transition::ScanSessionStatus::InProgress {
                return Err(crate::errors::VoomError::Other(
                    format!(
                        "scan session {session} is not in_progress (status: {:?})",
                        row.status,
                    )
                    .into(),
                ));
            }
            row.last_heartbeat_at = chrono::Utc::now();
            row.roots.clone()
        };

        // Step 2: collect the New-this-session IDs (released before acquiring other locks).
        let new_ids: std::collections::HashSet<Uuid> = self
            .new_in_session
            .lock()
            .get(&session)
            .cloned()
            .unwrap_or_default();

        // Step 3: identify move pairs (read-only pass; releases all locks when done).
        // A move pair is (candidate_id, new_id, old_path, new_path):
        //   candidate = active file under roots not seen this session with expected_hash
        //   new_id    = a New-this-session file whose content_hash == candidate.expected_hash
        let move_pairs: Vec<(Uuid, Uuid, std::path::PathBuf, std::path::PathBuf)> = {
            let last_seen = self.last_seen.lock();
            let files = self.files.lock();

            let new_by_hash: HashMap<String, (Uuid, std::path::PathBuf, u64)> = files
                .iter()
                .filter(|(id, _)| new_ids.contains(id))
                .filter_map(|(id, f)| {
                    f.content_hash
                        .as_ref()
                        .map(|h| (h.clone(), (*id, f.path.clone(), f.size)))
                })
                .collect();

            files
                .values()
                .filter(|f| {
                    f.status == FileStatus::Active
                        && roots.iter().any(|r| f.path.starts_with(r))
                        && last_seen.get(&f.path) != Some(&session)
                        && f.expected_hash.is_some()
                })
                .filter_map(|f| {
                    let hash = f.expected_hash.as_deref()?;
                    let (new_id, new_path, _) = new_by_hash.get(hash)?;
                    Some((f.id, *new_id, f.path.clone(), new_path.clone()))
                })
                .collect()
        };

        // Step 4: apply promotions one by one. Each iteration acquires/releases locks.
        let mut promoted_moves = 0u32;
        for (candidate_id, new_id, old_path, new_path) in &move_pairs {
            // Read the New row's data.
            let (new_size, new_hash) = {
                let files = self.files.lock();
                match files.get(new_id) {
                    Some(f) => (f.size, f.content_hash.clone()),
                    None => continue,
                }
            };

            // Remove the New row.
            self.files.lock().remove(new_id);

            // Update the candidate row to the new path.
            {
                let mut files = self.files.lock();
                if let Some(candidate) = files.get_mut(candidate_id) {
                    candidate.path = new_path.clone();
                    candidate.size = new_size;
                    candidate.content_hash = new_hash.clone();
                    candidate.status = FileStatus::Active;
                }
            }

            // Update last_seen so the missing pass below skips the new path.
            self.last_seen.lock().insert(new_path.clone(), session);

            // Record a move transition.
            let move_tx = FileTransition::new(
                *candidate_id,
                new_path.clone(),
                new_hash.unwrap_or_default(),
                new_size,
                TransitionSource::Discovery,
            )
            .with_from_path(old_path.clone())
            .with_detail("detected_move");
            self.transitions.lock().push(move_tx);

            promoted_moves += 1;
        }

        // Step 5: missing pass (last_seen then files, in order).
        let missing = {
            let last_seen = self.last_seen.lock();
            let mut files = self.files.lock();
            let mut count = 0u32;
            for f in files.values_mut() {
                if f.status != FileStatus::Active {
                    continue;
                }
                let under_root = roots.iter().any(|r| f.path.starts_with(r));
                if !under_root {
                    continue;
                }
                if last_seen.get(&f.path) == Some(&session) {
                    continue;
                }
                f.status = FileStatus::Missing;
                count += 1;
            }
            count
        };

        // Step 6: flip session status to Completed.
        if let Some(row) = self.sessions.lock().get_mut(&session) {
            row.status = crate::transition::ScanSessionStatus::Completed;
            row.last_heartbeat_at = chrono::Utc::now();
        }

        Ok(ScanFinishOutcome::new(missing, promoted_moves))
    }

    fn cancel_scan_session(&self, session: crate::transition::ScanSessionId) -> Result<()> {
        let mut sessions = self.sessions.lock();
        if let Some(row) = sessions.get_mut(&session) {
            if row.status == crate::transition::ScanSessionStatus::InProgress {
                row.status = crate::transition::ScanSessionStatus::Cancelled;
                row.last_heartbeat_at = chrono::Utc::now();
            }
        }
        Ok(())
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

    fn list_files_due_for_verification(
        &self,
        cutoff: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<MediaFile>> {
        let files = self.files.lock();
        let verifications = self.verifications.lock();
        let mut due: Vec<MediaFile> = files
            .values()
            .filter(|file| file.status == FileStatus::Active)
            .filter(|file| {
                let file_id = file.id.to_string();
                verifications
                    .iter()
                    .filter(|record| record.file_id == file_id)
                    .max_by(|a, b| {
                        a.verified_at
                            .cmp(&b.verified_at)
                            .then_with(|| a.id.cmp(&b.id))
                    })
                    .is_none_or(|record| record.verified_at < cutoff)
            })
            .cloned()
            .collect();
        due.sort_by_key(|file| file.id);
        Ok(due)
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

        Ok(IntegritySummary::new(IntegritySummaryInput {
            total_files,
            never_verified,
            stale,
            with_errors,
            with_warnings,
            hash_mismatches,
        }))
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
mod scan_session_tests {
    use super::*;
    use crate::storage::FileStorage;
    use crate::transition::{DiscoveredFile, IngestDecision};

    #[test]
    fn in_memory_store_stub_recovery() {
        let store = InMemoryStore::new();
        let session1 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/m/a.mkv"), 100, "h-a".to_string());
        let d1 = store.ingest_discovered_file(session1, &df).unwrap();
        let new_id = match d1 {
            IngestDecision::New { file_id, .. } => file_id,
            other => panic!("expected New, got {other:?}"),
        };

        // Cancel the session (simulating a crash).
        store.cancel_scan_session(session1).unwrap();

        // Cancelled session is no longer in_progress, so begin proceeds without issue.
        let session2 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();

        // Re-ingest the same path — should detect the stub (from session1,
        // which is now cancelled) and re-process as New.
        let d2 = store.ingest_discovered_file(session2, &df).unwrap();
        match d2 {
            IngestDecision::New {
                file_id,
                needs_introspection,
            } => {
                assert!(needs_introspection);
                // Stub recovery must produce a fresh file id.
                assert_ne!(
                    file_id, new_id,
                    "stub recovery must produce a fresh file id"
                );
            }
            other => panic!("expected New on stub recovery, got {other:?}"),
        }
    }

    #[test]
    fn in_memory_store_stub_recovery_fires_with_changed_hash() {
        use crate::transition::{DiscoveredFile, IngestDecision};
        use std::path::PathBuf;

        let store = InMemoryStore::new();
        // Session 1: ingest /m/a.mkv as New with hash h-old.
        let session1 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();
        let df_old = DiscoveredFile::new(PathBuf::from("/m/a.mkv"), 100, "h-old".to_string());
        store.ingest_discovered_file(session1, &df_old).unwrap();
        store.cancel_scan_session(session1).unwrap();

        // Session 2: re-ingest same path with DIFFERENT hash.
        let session2 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();
        let df_new = DiscoveredFile::new(PathBuf::from("/m/a.mkv"), 100, "h-new".to_string());
        let decision = store.ingest_discovered_file(session2, &df_new).unwrap();
        // Must be New (stub recovery), not ExternallyChanged.
        assert!(
            matches!(decision, IngestDecision::New { .. }),
            "stub recovery with changed hash must produce New; got {decision:?}"
        );
    }

    #[test]
    fn in_memory_store_reconcile_wrapper_exercises_session_api() {
        use crate::storage::FileStorage;
        use crate::transition::DiscoveredFile;
        use std::path::PathBuf;

        let store = InMemoryStore::new();
        let discovered = vec![DiscoveredFile::new(
            PathBuf::from("/m/a.mkv"),
            100,
            "h-a".to_string(),
        )];
        let result = store
            .reconcile_discovered_files(&discovered, &[PathBuf::from("/m")])
            .unwrap();
        assert_eq!(result.new_files, 1, "wrapper must produce a New count");
        assert_eq!(result.needs_introspection.len(), 1);
    }

    #[test]
    fn in_memory_store_unchanged_after_completed_session() {
        let store = InMemoryStore::new();
        let session1 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/m/b.mkv"), 200, "h-b".to_string());
        let d1 = store.ingest_discovered_file(session1, &df).unwrap();
        let orig_id = match d1 {
            IngestDecision::New { file_id, .. } => file_id,
            other => panic!("expected New, got {other:?}"),
        };
        store.finish_scan_session(session1).unwrap();

        // After a completed session, re-ingesting the same file should be Unchanged.
        let session2 = store.begin_scan_session(&[PathBuf::from("/m")]).unwrap();
        let d2 = store.ingest_discovered_file(session2, &df).unwrap();
        match d2 {
            IngestDecision::Unchanged { file_id } => {
                assert_eq!(
                    file_id, orig_id,
                    "Unchanged must preserve the original file id"
                );
            }
            other => panic!("expected Unchanged, got {other:?}"),
        }
    }
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

impl TranscodeOutcomeStorage for InMemoryStore {
    fn insert_transcode_outcome(&self, outcome: &TranscodeOutcome) -> Result<()> {
        self.transcode_outcomes.lock().push(outcome.clone());
        Ok(())
    }

    fn list_transcode_outcomes(
        &self,
        filters: &TranscodeOutcomeFilters,
    ) -> Result<Vec<TranscodeOutcome>> {
        let mut outcomes: Vec<TranscodeOutcome> = self
            .transcode_outcomes
            .lock()
            .iter()
            .filter(|outcome| matches_transcode_outcome_filter(outcome, filters))
            .cloned()
            .collect();
        outcomes.sort_by(|a, b| {
            b.completed_at
                .cmp(&a.completed_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        if let Some(limit) = filters.limit {
            outcomes.truncate(limit as usize);
        }
        Ok(outcomes)
    }

    fn latest_outcome_for_file(&self, file_id: &str) -> Result<Option<TranscodeOutcome>> {
        let filters = TranscodeOutcomeFilters {
            file_id: Some(file_id.to_string()),
            limit: Some(1),
        };
        Ok(self.list_transcode_outcomes(&filters)?.into_iter().next())
    }
}

impl EstimateStorage for InMemoryStore {
    fn insert_estimate_run(&self, run: &EstimateRun) -> Result<()> {
        self.estimate_runs.lock().push(run.clone());
        Ok(())
    }

    fn get_estimate_run(&self, id: &Uuid) -> Result<Option<EstimateRun>> {
        Ok(self
            .estimate_runs
            .lock()
            .iter()
            .find(|run| run.id == *id)
            .cloned())
    }

    fn list_estimate_runs(&self, limit: u32) -> Result<Vec<EstimateRun>> {
        let mut runs = self.estimate_runs.lock().clone();
        runs.sort_by(|a, b| {
            b.estimated_at
                .cmp(&a.estimated_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        runs.truncate(limit as usize);
        Ok(runs)
    }

    fn insert_cost_model_sample(&self, sample: &CostModelSample) -> Result<()> {
        self.cost_model_samples.lock().push(sample.clone());
        Ok(())
    }

    fn list_cost_model_samples(
        &self,
        filters: &CostModelSampleFilters,
    ) -> Result<Vec<CostModelSample>> {
        let mut samples: Vec<CostModelSample> = self
            .cost_model_samples
            .lock()
            .iter()
            .filter(|sample| matches_cost_model_filter(sample, filters))
            .cloned()
            .collect();
        samples.sort_by(|a, b| {
            b.completed_at
                .cmp(&a.completed_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        if let Some(limit) = filters.limit {
            samples.truncate(limit as usize);
        }
        Ok(samples)
    }
}

fn matches_cost_model_filter(sample: &CostModelSample, filters: &CostModelSampleFilters) -> bool {
    if let Some(key) = filters.key.as_ref() {
        if sample.key != *key {
            return false;
        }
    }
    true
}

fn matches_transcode_outcome_filter(
    outcome: &TranscodeOutcome,
    filters: &TranscodeOutcomeFilters,
) -> bool {
    if let Some(file_id) = filters.file_id.as_ref() {
        if outcome.file_id != *file_id {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod verification_storage_tests {
    use super::*;
    use crate::storage::{FileStorage, VerificationStorage};
    use crate::verification::VerificationRecordInput;

    fn media_file(path: &str) -> MediaFile {
        MediaFile::new(PathBuf::from(path))
    }

    #[test]
    fn list_files_due_for_verification_includes_never_verified_files() {
        let file = media_file("/media/never.mkv");
        let file_id = file.id;
        let store = InMemoryStore::new().with_file(file);

        let due = store
            .list_files_due_for_verification(chrono::Utc::now())
            .expect("due files");

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, file_id);
    }

    #[test]
    fn list_files_due_for_verification_filters_by_latest_verification() {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
        let stale_file = media_file("/media/stale.mkv");
        let fresh_file = media_file("/media/fresh.mkv");
        let stale_id = stale_file.id;
        let fresh_id = fresh_file.id;
        let store = InMemoryStore::new()
            .with_file(stale_file)
            .with_file(fresh_file)
            .with_verification(VerificationRecord::new(VerificationRecordInput {
                id: Uuid::new_v4(),
                file_id: stale_id.to_string(),
                verified_at: cutoff - chrono::Duration::seconds(1),
                mode: VerificationMode::Quick,
                outcome: VerificationOutcome::Ok,
                error_count: 0,
                warning_count: 0,
                content_hash: None,
                details: None,
            }))
            .with_verification(VerificationRecord::new(VerificationRecordInput {
                id: Uuid::new_v4(),
                file_id: fresh_id.to_string(),
                verified_at: cutoff + chrono::Duration::seconds(1),
                mode: VerificationMode::Quick,
                outcome: VerificationOutcome::Ok,
                error_count: 0,
                warning_count: 0,
                content_hash: None,
                details: None,
            }));

        let due = store
            .list_files_due_for_verification(cutoff)
            .expect("due files");
        let due_ids: Vec<_> = due.iter().map(|file| file.id).collect();

        assert_eq!(due_ids, vec![stale_id]);
    }

    #[test]
    fn list_files_due_for_verification_excludes_missing_files() {
        let file = media_file("/media/missing.mkv");
        let file_id = file.id;
        let store = InMemoryStore::new().with_file(file);
        store.mark_missing(&file_id).expect("mark missing");

        let due = store
            .list_files_due_for_verification(chrono::Utc::now())
            .expect("due files");

        assert!(due.is_empty());
    }
}

#[cfg(test)]
mod transcode_outcome_storage_tests {
    use super::*;
    use crate::storage::{TranscodeOutcomeFilters, TranscodeOutcomeStorage};
    use crate::transcode::TranscodeOutcome;

    fn outcome(
        id: u128,
        file_id: &str,
        completed_at: chrono::DateTime<chrono::Utc>,
    ) -> TranscodeOutcome {
        TranscodeOutcome {
            id: Uuid::from_u128(id),
            file_id: file_id.to_string(),
            target_vmaf: Some(95),
            achieved_vmaf: Some(94.8),
            crf_used: Some(22),
            bitrate_used: Some("3200k".to_string()),
            iterations: 3,
            sample_strategy: crate::plan::SampleStrategy::Uniform {
                count: 5,
                duration: "10s".to_string(),
            },
            fallback_used: false,
            completed_at,
        }
    }

    #[test]
    fn list_transcode_outcomes_filters_file_newest_first_with_id_tiebreaker() {
        let file_id = Uuid::new_v4().to_string();
        let other_file_id = Uuid::new_v4().to_string();
        let completed_at = chrono::Utc::now();
        let store = InMemoryStore::new()
            .with_transcode_outcome(outcome(1, &file_id, completed_at))
            .with_transcode_outcome(outcome(3, &file_id, completed_at))
            .with_transcode_outcome(outcome(
                2,
                &file_id,
                completed_at - chrono::Duration::minutes(1),
            ))
            .with_transcode_outcome(outcome(4, &other_file_id, completed_at));

        let listed = store
            .list_transcode_outcomes(&TranscodeOutcomeFilters {
                file_id: Some(file_id),
                limit: None,
            })
            .expect("list outcomes");
        let ids: Vec<_> = listed.iter().map(|record| record.id).collect();

        assert_eq!(
            ids,
            vec![Uuid::from_u128(3), Uuid::from_u128(1), Uuid::from_u128(2)]
        );
    }

    #[test]
    fn latest_outcome_for_file_returns_newest_with_id_tiebreaker() {
        let file_id = Uuid::new_v4().to_string();
        let completed_at = chrono::Utc::now();
        let store = InMemoryStore::new()
            .with_transcode_outcome(outcome(1, &file_id, completed_at))
            .with_transcode_outcome(outcome(2, &file_id, completed_at));

        let latest = store
            .latest_outcome_for_file(&file_id)
            .expect("latest outcome")
            .expect("some outcome");

        assert_eq!(latest.id, Uuid::from_u128(2));
    }
}
