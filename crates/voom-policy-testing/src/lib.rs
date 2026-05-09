//! Test support for evaluating VOOM policies against JSON fixtures.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;
use thiserror::Error;
use voom_domain::capability_map::CapabilityMap;
use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};
use voom_domain::media::{MediaFile, Track, TrackType};
use voom_domain::plan::{ActionParams, OperationType, Plan};

/// A JSON fixture describing media metadata without running introspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub path: PathBuf,
    pub container: voom_domain::media::Container,
    pub duration: f64,
    pub size: u64,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub capabilities: Option<CapabilityFixture>,
}

impl Fixture {
    /// Load a fixture from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or does not contain a
    /// valid fixture JSON document.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PolicyTestError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| PolicyTestError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&contents).map_err(|source| PolicyTestError::ParseJson {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Convert this fixture into the domain media type used by the evaluator.
    #[must_use]
    pub fn to_media_file(&self) -> MediaFile {
        let mut file = MediaFile::new(self.path.clone())
            .with_container(self.container)
            .with_duration(self.duration)
            .with_tracks(self.tracks.clone());
        file.size = self.size;
        file
    }

    /// Return this fixture's capabilities override, or the standard default map.
    #[must_use]
    pub fn capabilities_or_default(&self) -> CapabilityMap {
        self.capabilities
            .as_ref()
            .map_or_else(all_capabilities, CapabilityFixture::to_capability_map)
    }
}

/// JSON-friendly executor capability overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityFixture {
    #[serde(default)]
    pub executors: Vec<ExecutorCapabilityFixture>,
}

impl CapabilityFixture {
    /// Convert JSON fixture capabilities to the evaluator's capability map.
    #[must_use]
    pub fn to_capability_map(&self) -> CapabilityMap {
        let mut map = CapabilityMap::new();
        for executor in &self.executors {
            map.register(ExecutorCapabilitiesEvent::new(
                executor.name.clone(),
                CodecCapabilities::new(executor.decoders.clone(), executor.encoders.clone()),
                executor.formats.clone(),
                executor.hw_accels.clone(),
            ));
        }
        map
    }
}

/// JSON-friendly capability entry for one executor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutorCapabilityFixture {
    pub name: String,
    #[serde(default)]
    pub decoders: Vec<String>,
    #[serde(default)]
    pub encoders: Vec<String>,
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub hw_accels: Vec<String>,
}

/// A JSON test suite describing one policy and a set of fixture cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuite {
    pub policy: PathBuf,
    pub cases: Vec<TestCase>,
}

impl TestSuite {
    /// Load a test suite from a JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or does not contain a
    /// valid test suite JSON document.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, PolicyTestError> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| PolicyTestError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&contents).map_err(|source| PolicyTestError::ParseJson {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// One fixture-backed policy test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub fixture: PathBuf,
    #[serde(default)]
    pub expect: Assertions,
    #[serde(default)]
    pub snapshot: Option<PathBuf>,
    #[serde(default)]
    pub capabilities: Option<CapabilityFixture>,
}

/// Flat optional assertion set used by JSON test cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assertions {
    #[serde(default)]
    pub phases_run: Option<Vec<String>>,
    #[serde(default)]
    pub phases_skipped: Option<HashMap<String, String>>,
    #[serde(default)]
    pub audio_tracks_kept: Option<usize>,
    #[serde(default)]
    pub audio_tracks_synthesized: Option<usize>,
    #[serde(default)]
    pub subtitle_tracks_kept: Option<usize>,
    #[serde(default)]
    pub video_codec: Option<String>,
    #[serde(default)]
    pub no_warnings: Option<bool>,
}

impl Assertions {
    /// Run every populated assertion against the evaluator plans.
    ///
    /// # Errors
    ///
    /// Returns the first assertion failure encountered.
    pub fn check(&self, plans: &[Plan]) -> Result<(), AssertionFailure> {
        if let Some(expected) = &self.phases_run {
            assert_phases_run(plans, expected)?;
        }
        if let Some(expected) = &self.phases_skipped {
            assert_phases_skipped(plans, expected)?;
        }
        if let Some(expected) = self.audio_tracks_kept {
            assert_audio_tracks_kept(plans, expected)?;
        }
        if let Some(expected) = self.audio_tracks_synthesized {
            assert_audio_tracks_synthesized(plans, expected)?;
        }
        if let Some(expected) = self.subtitle_tracks_kept {
            assert_subtitle_tracks_kept(plans, expected)?;
        }
        if let Some(expected) = &self.video_codec {
            assert_video_codec(plans, expected)?;
        }
        if self.no_warnings == Some(true) {
            assert_no_warnings(plans)?;
        }
        Ok(())
    }
}

/// Errors returned while loading JSON policy test inputs.
#[derive(Debug, Error)]
pub enum PolicyTestError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse JSON in {path}: {source}")]
    ParseJson {
        path: PathBuf,
        source: serde_json::Error,
    },
}

/// Result of comparing a plan snapshot file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOutcome {
    Matched,
    Updated,
}

/// Errors returned while rendering or comparing plan snapshots.
#[derive(Debug, Error)]
pub enum SnapshotFailure {
    #[error("failed to serialize plans for snapshot: {source}")]
    Serialize { source: serde_json::Error },
    #[error("failed to read snapshot {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write snapshot {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("snapshot does not exist: {path}; rerun with --update to create it")]
    Missing { path: PathBuf },
    #[error("snapshot mismatch for {path}\n{diff}")]
    Mismatch { path: PathBuf, diff: String },
}

/// A single failed policy assertion.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct AssertionFailure {
    message: String,
}

impl AssertionFailure {
    #[must_use]
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Convert plans to canonical JSON for stable snapshot files.
///
/// # Errors
///
/// Returns an error when the plan list cannot be serialized to JSON.
pub fn canonicalize_plans_for_snapshot(plans: &[Plan]) -> Result<Value, SnapshotFailure> {
    let mut value =
        serde_json::to_value(plans).map_err(|source| SnapshotFailure::Serialize { source })?;
    scrub_nondeterministic_fields(&mut value);
    Ok(value)
}

/// Render plans as canonical pretty JSON with a trailing newline.
///
/// # Errors
///
/// Returns an error when the plan list cannot be serialized to JSON.
pub fn snapshot_json(plans: &[Plan]) -> Result<String, SnapshotFailure> {
    let value = canonicalize_plans_for_snapshot(plans)?;
    let mut json = serde_json::to_string_pretty(&value)
        .map_err(|source| SnapshotFailure::Serialize { source })?;
    json.push('\n');
    Ok(json)
}

/// Compare evaluated plans with a snapshot file, or update that file in place.
///
/// # Errors
///
/// Returns an error when snapshots cannot be read or written, or when the
/// existing snapshot differs and `update` is false.
pub fn assert_snapshot_file(
    plans: &[Plan],
    snapshot: &Path,
    update: bool,
) -> Result<SnapshotOutcome, SnapshotFailure> {
    let actual = snapshot_json(plans)?;
    if update {
        write_snapshot(snapshot, &actual)?;
        return Ok(SnapshotOutcome::Updated);
    }
    let expected = read_snapshot(snapshot)?;
    if expected == actual {
        return Ok(SnapshotOutcome::Matched);
    }
    Err(SnapshotFailure::Mismatch {
        path: snapshot.to_path_buf(),
        diff: snapshot_diff(&expected, &actual),
    })
}

fn write_snapshot(snapshot: &Path, contents: &str) -> Result<(), SnapshotFailure> {
    if let Some(parent) = snapshot.parent() {
        fs::create_dir_all(parent).map_err(|source| SnapshotFailure::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(snapshot, contents).map_err(|source| SnapshotFailure::Write {
        path: snapshot.to_path_buf(),
        source,
    })
}

fn read_snapshot(snapshot: &Path) -> Result<String, SnapshotFailure> {
    fs::read_to_string(snapshot).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            SnapshotFailure::Missing {
                path: snapshot.to_path_buf(),
            }
        } else {
            SnapshotFailure::Read {
                path: snapshot.to_path_buf(),
                source,
            }
        }
    })
}

fn snapshot_diff(expected: &str, actual: &str) -> String {
    TextDiff::from_lines(expected, actual)
        .unified_diff()
        .header("expected", "actual")
        .to_string()
}

fn scrub_nondeterministic_fields(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                scrub_nondeterministic_fields(item);
            }
        }
        Value::Object(fields) => {
            for (key, child) in fields {
                if is_nondeterministic_key(key) {
                    *child = canonical_value_for_key(key);
                } else {
                    scrub_nondeterministic_fields(child);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_nondeterministic_key(key: &str) -> bool {
    key == "id" || key == "session_id" || key.ends_with("_at") || key.ends_with("_timestamp")
}

fn canonical_value_for_key(key: &str) -> Value {
    if key.ends_with("_at") || key.ends_with("_timestamp") {
        Value::String("1970-01-01T00:00:00Z".to_string())
    } else {
        Value::Null
    }
}

/// Return a capability map with the standard VOOM executors available.
#[must_use]
pub fn all_capabilities() -> CapabilityMap {
    let mut map = CapabilityMap::new();
    map.register(ExecutorCapabilitiesEvent::new(
        "ffmpeg-executor",
        CodecCapabilities::new(
            vec![
                "h264".to_string(),
                "hevc".to_string(),
                "aac".to_string(),
                "opus".to_string(),
                "subrip".to_string(),
            ],
            vec![
                "libx264".to_string(),
                "libx265".to_string(),
                "libopus".to_string(),
                "aac".to_string(),
            ],
        ),
        vec![
            "matroska".to_string(),
            "mp4".to_string(),
            "webm".to_string(),
            "mov".to_string(),
            "mpegts".to_string(),
        ],
        vec![],
    ));
    map.register(ExecutorCapabilitiesEvent::new(
        "mkvtoolnix-executor",
        CodecCapabilities::empty(),
        vec!["matroska".to_string()],
        vec![],
    ));
    map
}

/// Assert that all named phases produced non-skipped plans.
pub fn assert_phases_run(plans: &[Plan], expected: &[String]) -> Result<(), AssertionFailure> {
    for phase in expected {
        let Some(plan) = plans.iter().find(|plan| plan.phase_name == *phase) else {
            return Err(AssertionFailure::new(format!(
                "expected phase '{phase}' to run, but no plan exists for it"
            )));
        };
        if plan.is_skipped() {
            return Err(AssertionFailure::new(format!(
                "expected phase '{phase}' to run, but it skipped: {}",
                plan.skip_reason.as_deref().unwrap_or("unknown reason")
            )));
        }
    }
    Ok(())
}

/// Assert that named phases skipped with reasons containing the expected text.
pub fn assert_phases_skipped(
    plans: &[Plan],
    expected: &HashMap<String, String>,
) -> Result<(), AssertionFailure> {
    for (phase, reason) in expected {
        let Some(plan) = plans.iter().find(|plan| plan.phase_name == *phase) else {
            return Err(AssertionFailure::new(format!(
                "expected phase '{phase}' to skip, but no plan exists for it"
            )));
        };
        let Some(actual) = &plan.skip_reason else {
            return Err(AssertionFailure::new(format!(
                "expected phase '{phase}' to skip with reason containing '{reason}'"
            )));
        };
        if !actual.contains(reason) {
            return Err(AssertionFailure::new(format!(
                "expected skipped phase '{phase}' reason to contain '{reason}', got '{actual}'"
            )));
        }
    }
    Ok(())
}

/// Assert how many audio tracks remain after planned removals.
pub fn assert_audio_tracks_kept(plans: &[Plan], expected: usize) -> Result<(), AssertionFailure> {
    assert_tracks_kept(plans, expected, TrackKind::Audio)
}

/// Assert how many synthesized audio actions are planned.
pub fn assert_audio_tracks_synthesized(
    plans: &[Plan],
    expected: usize,
) -> Result<(), AssertionFailure> {
    let actual = plans
        .iter()
        .flat_map(|plan| &plan.actions)
        .filter(|action| action.operation == OperationType::SynthesizeAudio)
        .count();
    if actual != expected {
        return Err(AssertionFailure::new(format!(
            "expected {expected} synthesized audio tracks, got {actual}"
        )));
    }
    Ok(())
}

/// Assert how many subtitle tracks remain after planned removals.
pub fn assert_subtitle_tracks_kept(
    plans: &[Plan],
    expected: usize,
) -> Result<(), AssertionFailure> {
    assert_tracks_kept(plans, expected, TrackKind::Subtitle)
}

/// Assert the final planned video codec, considering video transcode actions.
pub fn assert_video_codec(plans: &[Plan], expected: &str) -> Result<(), AssertionFailure> {
    let mut actual = source_video_codec(plans)?;
    for action in plans.iter().flat_map(|plan| &plan.actions) {
        if action.operation != OperationType::TranscodeVideo {
            continue;
        }
        if let ActionParams::Transcode { codec, .. } = &action.parameters {
            actual = codec.clone();
        }
    }
    if actual != expected {
        return Err(AssertionFailure::new(format!(
            "expected video codec '{expected}', got '{actual}'"
        )));
    }
    Ok(())
}

/// Assert that no plan contains evaluator warnings.
pub fn assert_no_warnings(plans: &[Plan]) -> Result<(), AssertionFailure> {
    let warnings: Vec<String> = plans
        .iter()
        .flat_map(|plan| {
            plan.warnings
                .iter()
                .map(|warning| format!("{}: {warning}", plan.phase_name))
        })
        .collect();
    if warnings.is_empty() {
        return Ok(());
    }
    Err(AssertionFailure::new(format!(
        "expected no warnings, got {}",
        warnings.join("; ")
    )))
}

enum TrackKind {
    Audio,
    Subtitle,
}

fn assert_tracks_kept(
    plans: &[Plan],
    expected: usize,
    kind: TrackKind,
) -> Result<(), AssertionFailure> {
    let initial = source_tracks(plans)?
        .iter()
        .filter(|track| matches_track_kind(track.track_type, &kind))
        .count();
    let removed = plans
        .iter()
        .flat_map(|plan| &plan.actions)
        .filter(|action| action.operation == OperationType::RemoveTrack)
        .filter(|action| {
            if let ActionParams::RemoveTrack { track_type, .. } = action.parameters {
                matches_track_kind(track_type, &kind)
            } else {
                false
            }
        })
        .count();
    let actual = initial.saturating_sub(removed);
    if actual != expected {
        return Err(AssertionFailure::new(format!(
            "expected {expected} {} tracks kept, got {actual}",
            track_kind_name(&kind)
        )));
    }
    Ok(())
}

fn source_tracks(plans: &[Plan]) -> Result<&[Track], AssertionFailure> {
    plans
        .first()
        .map(|plan| plan.file.tracks.as_slice())
        .ok_or_else(|| AssertionFailure::new("expected plans, got an empty plan list"))
}

fn source_video_codec(plans: &[Plan]) -> Result<String, AssertionFailure> {
    source_tracks(plans)?
        .iter()
        .find(|track| track.track_type.is_video())
        .map(|track| track.codec.clone())
        .ok_or_else(|| AssertionFailure::new("expected a source video track"))
}

fn matches_track_kind(track_type: TrackType, kind: &TrackKind) -> bool {
    match kind {
        TrackKind::Audio => track_type.is_audio(),
        TrackKind::Subtitle => track_type.is_subtitle(),
    }
}

fn track_kind_name(kind: &TrackKind) -> &'static str {
    match kind {
        TrackKind::Audio => "audio",
        TrackKind::Subtitle => "subtitle",
    }
}
