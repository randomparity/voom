//! Types for tracking file lifecycle transitions and modification provenance.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::snapshot::MetadataSnapshot;
use crate::stats::ProcessingOutcome;

/// Whether a file is currently present and accessible.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FileStatus {
    /// File exists at its expected path.
    #[default]
    Active,
    /// File was not found at its expected path during the last scan.
    Missing,
    /// File was moved out of the library (e.g. by the verifier after a
    /// thorough-mode failure) and should be excluded from normal flows.
    Quarantined,
}

impl FileStatus {
    /// Returns the canonical string representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FileStatus::Active => "active",
            FileStatus::Missing => "missing",
            FileStatus::Quarantined => "quarantined",
        }
    }

    /// Parse from a string, returning `None` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(FileStatus::Active),
            "missing" => Some(FileStatus::Missing),
            "quarantined" => Some(FileStatus::Quarantined),
            _ => None,
        }
    }
}

/// What initiated a file transition.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransitionSource {
    /// File was discovered (created or first seen) during a scan.
    Discovery,
    /// Voom modified the file as part of executing a plan.
    Voom,
    /// A change was detected that voom did not initiate (e.g. manual edit).
    External,
    /// Source is not known.
    #[default]
    Unknown,
}

impl TransitionSource {
    /// Returns the canonical string representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionSource::Discovery => "discovery",
            TransitionSource::Voom => "voom",
            TransitionSource::External => "external",
            TransitionSource::Unknown => "unknown",
        }
    }

    /// Parse from a string, returning `None` for unrecognized values.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "discovery" => Some(TransitionSource::Discovery),
            "voom" => Some(TransitionSource::Voom),
            "external" => Some(TransitionSource::External),
            "unknown" => Some(TransitionSource::Unknown),
            _ => None,
        }
    }
}

/// A recorded change in a file's content hash, with provenance information.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTransition {
    /// Unique identifier for this transition record.
    pub id: Uuid,
    /// The ID of the `MediaFile` this transition belongs to.
    pub file_id: Uuid,
    /// Path of the file after the transition.
    pub path: PathBuf,
    /// Path of the file before the transition. Set only when the path
    /// changed; lets path-based history queries find the row by either side.
    pub from_path: Option<PathBuf>,
    /// Content hash before the change, if known.
    pub from_hash: Option<String>,
    /// Content hash after the change.
    pub to_hash: String,
    /// File size before the change, if known.
    pub from_size: Option<u64>,
    /// File size after the change.
    pub to_size: u64,
    /// What caused this transition.
    pub source: TransitionSource,
    /// Optional human-readable detail about the source (e.g. tool name, plan phase).
    pub source_detail: Option<String>,
    /// The plan that produced this change, if applicable.
    pub plan_id: Option<Uuid>,
    /// When this transition was recorded.
    pub created_at: DateTime<Utc>,
    /// Processing duration in milliseconds (only for source=Voom transitions).
    pub duration_ms: Option<u64>,
    /// Number of actions executed (only for source=Voom transitions).
    pub actions_taken: Option<u32>,
    /// Number of tracks modified (only for source=Voom transitions).
    pub tracks_modified: Option<u32>,
    /// Processing outcome (only for source=Voom transitions).
    pub outcome: Option<ProcessingOutcome>,
    /// Policy name that produced this transition (only for source=Voom transitions).
    pub policy_name: Option<String>,
    /// Phase name within the policy (only for source=Voom transitions).
    pub phase_name: Option<String>,
    /// Snapshot of the file's media properties after this transition completed.
    /// For `source=Voom` transitions this reflects the post-processing state.
    /// To determine the pre-processing state, read the snapshot from the
    /// preceding transition in the file's history chain.
    pub metadata_snapshot: Option<MetadataSnapshot>,
    /// Error message when outcome is failure.
    pub error_message: Option<String>,
    /// Session UUID linking transitions to a single `voom process` run.
    pub session_id: Option<Uuid>,
}

impl FileTransition {
    /// Create a new transition record with just the post-change state.
    ///
    /// For discovery transitions where the previous state is unknown, use this
    /// constructor and optionally call [`with_from`](Self::with_from) to set the
    /// prior hash and size.
    #[must_use]
    pub fn new(
        file_id: Uuid,
        path: PathBuf,
        to_hash: String,
        to_size: u64,
        source: TransitionSource,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            file_id,
            path,
            from_path: None,
            from_hash: None,
            to_hash,
            from_size: None,
            to_size,
            source,
            source_detail: None,
            plan_id: None,
            created_at: Utc::now(),
            duration_ms: None,
            actions_taken: None,
            tracks_modified: None,
            outcome: None,
            policy_name: None,
            phase_name: None,
            metadata_snapshot: None,
            error_message: None,
            session_id: None,
        }
    }

    /// Set the error message for a failed transition.
    #[must_use]
    pub fn with_error_message(mut self, message: impl Into<String>) -> Self {
        self.error_message = Some(message.into());
        self
    }

    /// Set the session ID linking this transition to a `voom process` run.
    #[must_use]
    pub fn with_session_id(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Set the prior hash and size (pre-change state).
    #[must_use]
    pub fn with_from(mut self, hash: Option<String>, size: Option<u64>) -> Self {
        self.from_hash = hash;
        self.from_size = size;
        self
    }

    /// Set the prior path (pre-change state).
    #[must_use]
    pub fn with_from_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.from_path = Some(path.into());
        self
    }

    /// Set the `source_detail` field.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.source_detail = Some(detail.into());
        self
    }

    /// Set the `plan_id` field.
    #[must_use]
    pub fn with_plan_id(mut self, plan_id: Uuid) -> Self {
        self.plan_id = Some(plan_id);
        self
    }

    /// Attach processing statistics to this transition.
    #[must_use]
    pub fn with_processing(
        mut self,
        duration_ms: u64,
        actions_taken: u32,
        tracks_modified: u32,
        outcome: ProcessingOutcome,
        policy_name: impl Into<String>,
        phase_name: impl Into<String>,
    ) -> Self {
        self.duration_ms = Some(duration_ms);
        self.actions_taken = Some(actions_taken);
        self.tracks_modified = Some(tracks_modified);
        self.outcome = Some(outcome);
        self.policy_name = Some(policy_name.into());
        self.phase_name = Some(phase_name.into());
        self
    }

    /// Attach a metadata snapshot to this transition.
    #[must_use]
    pub fn with_metadata_snapshot(mut self, snapshot: MetadataSnapshot) -> Self {
        self.metadata_snapshot = Some(snapshot);
        self
    }
}

/// A file found during a filesystem scan, before reconciliation with stored state.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredFile {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// File size in bytes.
    pub size: u64,
    /// Content hash of the file.
    pub content_hash: String,
}

impl DiscoveredFile {
    /// Create a new discovered file record.
    #[must_use]
    pub fn new(path: PathBuf, size: u64, content_hash: String) -> Self {
        Self {
            path,
            size,
            content_hash,
        }
    }
}

/// Summary of outcomes from a batch reconciliation pass.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconcileResult {
    /// Files that are new and were not previously tracked.
    pub new_files: u32,
    /// Files whose content hash and path are unchanged.
    pub unchanged: u32,
    /// Files that were moved (same hash, different path).
    pub moved: u32,
    /// Files that changed without voom's involvement.
    pub external_changes: u32,
    /// Previously tracked files that could not be found.
    pub missing: u32,
    /// Paths that need introspection (new, moved, externally changed).
    pub needs_introspection: Vec<PathBuf>,
}

/// Outcome of a [`FileStorage::finish_scan_session`] call.
///
/// Returned instead of a bare `u32` so that move-promotion counts can be
/// threaded back to the caller alongside the missing count.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanFinishOutcome {
    /// Files that were not seen this session and were marked `missing`.
    pub missing: u32,
    /// Files that were ingested as `New` this session but were retroactively
    /// promoted to `Moved` during the finish pass (because an `active` file
    /// with a matching `expected_hash` was found to be absent).
    pub promoted_moves: u32,
}

impl ScanFinishOutcome {
    /// Create a new outcome with both counts.
    #[must_use]
    pub fn new(missing: u32, promoted_moves: u32) -> Self {
        Self {
            missing,
            promoted_moves,
        }
    }
}

/// Identifier for a scan session. Newtype around `Uuid` so callers can't
/// accidentally mix scan session IDs with the unrelated `voom process`
/// `session_id` that lives on `plans` and `file_transitions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanSessionId(Uuid);

impl ScanSessionId {
    /// Generate a fresh random session ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Borrow the inner UUID.
    #[must_use]
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ScanSessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for ScanSessionId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for ScanSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Lifecycle state of a [`ScanSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanSessionStatus {
    /// Session is open and ingesting files.
    InProgress,
    /// Session completed successfully; missing-file pass has run.
    Completed,
    /// Session was cancelled; no file was marked missing.
    Cancelled,
}

impl ScanSessionStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScanSessionStatus::InProgress => "in_progress",
            ScanSessionStatus::Completed => "completed",
            ScanSessionStatus::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "in_progress" => Some(ScanSessionStatus::InProgress),
            "completed" => Some(ScanSessionStatus::Completed),
            "cancelled" => Some(ScanSessionStatus::Cancelled),
            _ => None,
        }
    }
}

/// A scan session: a durable record bounding one "walk the filesystem and
/// reconcile" pass. Per-file ingestion happens while a session is `InProgress`;
/// missing-file detection runs only at `finish_scan_session`. See
/// `docs/superpowers/specs/2026-05-11-scan-sessions-design.md`.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSession {
    pub id: ScanSessionId,
    pub roots: Vec<std::path::PathBuf>,
    pub status: ScanSessionStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Outcome of a single per-file `ingest_discovered_file` call.
///
/// `Duplicate` indicates the same path was already ingested in this session
/// (e.g. overlapping scan roots). It is a no-op; the caller may use it to
/// count duplicates for reporting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IngestDecision {
    New {
        file_id: Uuid,
        needs_introspection: bool,
    },
    Unchanged {
        file_id: Uuid,
    },
    ExternallyChanged {
        file_id: Uuid,
        superseded: Uuid,
    },
    Moved {
        file_id: Uuid,
        from_path: std::path::PathBuf,
    },
    Duplicate {
        file_id: Uuid,
    },
}

impl IngestDecision {
    /// Return the file ID this decision pertains to.
    #[must_use]
    pub fn file_id(&self) -> Uuid {
        match self {
            IngestDecision::New { file_id, .. }
            | IngestDecision::Unchanged { file_id }
            | IngestDecision::ExternallyChanged { file_id, .. }
            | IngestDecision::Moved { file_id, .. }
            | IngestDecision::Duplicate { file_id } => *file_id,
        }
    }

    /// If this decision means "introspect this file next," return the path.
    ///
    /// Matches today's `ReconcileResult.needs_introspection` set: `New`,
    /// `ExternallyChanged`, and `Moved` all require introspection; `Unchanged`
    /// and `Duplicate` do not. See spec §6.3.
    #[must_use]
    pub fn needs_introspection_path(
        &self,
        ingested_path: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        match self {
            IngestDecision::New {
                needs_introspection: true,
                ..
            }
            | IngestDecision::ExternallyChanged { .. }
            | IngestDecision::Moved { .. } => Some(ingested_path.to_path_buf()),
            IngestDecision::New {
                needs_introspection: false,
                ..
            }
            | IngestDecision::Unchanged { .. }
            | IngestDecision::Duplicate { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_status_roundtrip() {
        for status in [
            FileStatus::Active,
            FileStatus::Missing,
            FileStatus::Quarantined,
        ] {
            assert_eq!(FileStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn test_file_status_quarantined_canonical_string() {
        assert_eq!(FileStatus::Quarantined.as_str(), "quarantined");
        assert_eq!(
            FileStatus::parse("quarantined"),
            Some(FileStatus::Quarantined)
        );
    }

    #[test]
    fn test_file_status_parse_returns_none_for_unknown() {
        assert_eq!(FileStatus::parse("garbage"), None);
        assert_eq!(FileStatus::parse(""), None);
    }

    #[test]
    fn test_file_status_default() {
        assert_eq!(FileStatus::default(), FileStatus::Active);
    }

    #[test]
    fn test_transition_source_roundtrip() {
        for source in [
            TransitionSource::Discovery,
            TransitionSource::Voom,
            TransitionSource::External,
            TransitionSource::Unknown,
        ] {
            assert_eq!(TransitionSource::parse(source.as_str()), Some(source));
        }
    }

    #[test]
    fn test_transition_source_parse_returns_none_for_unknown() {
        assert_eq!(TransitionSource::parse("garbage"), None);
        assert_eq!(TransitionSource::parse(""), None);
    }

    #[test]
    fn test_transition_source_default() {
        assert_eq!(TransitionSource::default(), TransitionSource::Unknown);
    }

    #[test]
    fn test_file_transition_builder() {
        let file_id = Uuid::new_v4();
        let plan_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Voom,
        )
        .with_from(Some("oldhash".into()), Some(1000))
        .with_detail("mkvtoolnix")
        .with_plan_id(plan_id);

        assert_eq!(t.file_id, file_id);
        assert_eq!(t.path, PathBuf::from("/movies/test.mkv"));
        assert_eq!(t.from_hash.as_deref(), Some("oldhash"));
        assert_eq!(t.to_hash, "newhash");
        assert_eq!(t.from_size, Some(1000));
        assert_eq!(t.to_size, 2000);
        assert_eq!(t.source, TransitionSource::Voom);
        assert_eq!(t.source_detail.as_deref(), Some("mkvtoolnix"));
        assert_eq!(t.plan_id, Some(plan_id));
    }

    #[test]
    fn test_file_transition_no_optional_fields() {
        let file_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Discovery,
        );
        assert!(t.source_detail.is_none());
        assert!(t.plan_id.is_none());
        assert!(t.from_hash.is_none());
        assert!(t.from_size.is_none());
    }

    #[test]
    fn test_reconcile_result_default() {
        let r = ReconcileResult::default();
        assert_eq!(r.new_files, 0);
        assert_eq!(r.unchanged, 0);
        assert_eq!(r.moved, 0);
        assert_eq!(r.external_changes, 0);
        assert_eq!(r.missing, 0);
    }

    #[test]
    fn test_discovered_file_new() {
        let df = DiscoveredFile::new(PathBuf::from("/movies/test.mkv"), 12345, "abc123".into());
        assert_eq!(df.path, PathBuf::from("/movies/test.mkv"));
        assert_eq!(df.size, 12345);
        assert_eq!(df.content_hash, "abc123");
    }

    #[test]
    fn test_file_transition_with_processing_stats() {
        let file_id = Uuid::new_v4();
        let plan_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Voom,
        )
        .with_from(Some("oldhash".into()), Some(3000))
        .with_detail("mkvtoolnix:normalize")
        .with_plan_id(plan_id)
        .with_processing(
            150,
            3,
            2,
            ProcessingOutcome::Success,
            "default",
            "normalize",
        );

        assert_eq!(t.duration_ms, Some(150));
        assert_eq!(t.actions_taken, Some(3));
        assert_eq!(t.tracks_modified, Some(2));
        assert_eq!(t.outcome, Some(ProcessingOutcome::Success));
        assert_eq!(t.policy_name.as_deref(), Some("default"));
        assert_eq!(t.phase_name.as_deref(), Some("normalize"));
    }

    #[test]
    fn test_file_transition_serde_roundtrip() {
        let file_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::External,
        )
        .with_from(Some("oldhash".into()), Some(1000))
        .with_detail("manual edit");

        let json = serde_json::to_string(&t).expect("serialize");
        let t2: FileTransition = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t2.file_id, t.file_id);
        assert_eq!(t2.to_hash, t.to_hash);
        assert_eq!(t2.source, TransitionSource::External);
        assert_eq!(t2.source_detail.as_deref(), Some("manual edit"));
    }

    #[test]
    fn test_file_status_serde_roundtrip() {
        let statuses = [
            FileStatus::Active,
            FileStatus::Missing,
            FileStatus::Quarantined,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).expect("serialize");
            let back: FileStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, status);
        }
    }

    #[test]
    fn test_file_transition_with_metadata_snapshot() {
        use crate::media::{Container, MediaFile, Track, TrackType};
        use crate::snapshot::MetadataSnapshot;

        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7200.5)
            .with_tracks(vec![
                Track::new(0, TrackType::Video, "hevc".into()),
                Track::new(1, TrackType::AudioMain, "truehd".into()),
            ]);

        let snap = MetadataSnapshot::from_media_file(&file);
        let t = FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Discovery,
        )
        .with_metadata_snapshot(snap.clone());

        assert_eq!(t.metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_transition_source_serde_roundtrip() {
        let sources = [
            TransitionSource::Discovery,
            TransitionSource::Voom,
            TransitionSource::External,
            TransitionSource::Unknown,
        ];
        for source in sources {
            let json = serde_json::to_string(&source).expect("serialize");
            let back: TransitionSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, source);
        }
    }

    #[test]
    fn transition_source_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_string(&TransitionSource::Discovery).unwrap(),
            "\"discovery\""
        );
        assert_eq!(
            serde_json::to_string(&TransitionSource::Voom).unwrap(),
            "\"voom\""
        );
        assert_eq!(
            serde_json::to_string(&TransitionSource::External).unwrap(),
            "\"external\""
        );
        assert_eq!(
            serde_json::to_string(&TransitionSource::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn test_scan_session_status_roundtrip() {
        for status in [
            ScanSessionStatus::InProgress,
            ScanSessionStatus::Completed,
            ScanSessionStatus::Cancelled,
        ] {
            assert_eq!(ScanSessionStatus::parse(status.as_str()), Some(status));
        }
    }

    #[test]
    fn test_scan_session_status_unknown_is_none() {
        assert_eq!(ScanSessionStatus::parse(""), None);
        assert_eq!(ScanSessionStatus::parse("bogus"), None);
    }

    #[test]
    fn test_ingest_decision_needs_introspection_path() {
        use std::path::PathBuf;
        use uuid::Uuid;

        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let path = PathBuf::from("/movies/a.mkv");

        let new = IngestDecision::New {
            file_id: id,
            needs_introspection: true,
        };
        assert_eq!(new.needs_introspection_path(&path), Some(path.clone()));

        let unchanged = IngestDecision::Unchanged { file_id: id };
        assert_eq!(unchanged.needs_introspection_path(&path), None);

        let moved = IngestDecision::Moved {
            file_id: id,
            from_path: PathBuf::from("/old.mkv"),
        };
        assert_eq!(moved.needs_introspection_path(&path), Some(path.clone()));

        let ext = IngestDecision::ExternallyChanged {
            file_id: id,
            superseded: other,
        };
        assert_eq!(ext.needs_introspection_path(&path), Some(path.clone()));

        let dup = IngestDecision::Duplicate { file_id: id };
        assert_eq!(dup.needs_introspection_path(&path), None);
    }

    #[test]
    fn test_ingest_decision_new_respects_needs_introspection_flag() {
        use std::path::PathBuf;
        use uuid::Uuid;

        let id = Uuid::new_v4();
        let path = PathBuf::from("/movies/a.mkv");

        let needed = IngestDecision::New {
            file_id: id,
            needs_introspection: true,
        };
        assert_eq!(needed.needs_introspection_path(&path), Some(path.clone()));

        let not_needed = IngestDecision::New {
            file_id: id,
            needs_introspection: false,
        };
        assert_eq!(not_needed.needs_introspection_path(&path), None);
    }

    #[test]
    fn test_ingest_decision_file_id_is_always_present() {
        use std::path::PathBuf;
        use uuid::Uuid;
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        for d in [
            IngestDecision::New {
                file_id: id,
                needs_introspection: true,
            },
            IngestDecision::Unchanged { file_id: id },
            IngestDecision::ExternallyChanged {
                file_id: id,
                superseded: other,
            },
            IngestDecision::Moved {
                file_id: id,
                from_path: PathBuf::from("/p"),
            },
            IngestDecision::Duplicate { file_id: id },
        ] {
            assert_eq!(d.file_id(), id);
        }
    }
}
