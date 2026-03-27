use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::{Container, MediaFile, TrackType};
use crate::safeguard::SafeguardViolation;

fn epoch() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

/// A plan produced by the policy evaluator for a single file in a single phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub file: MediaFile,
    pub policy_name: String,
    pub phase_name: String,
    pub actions: Vec<PlannedAction>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub safeguard_violations: Vec<SafeguardViolation>,
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub policy_hash: Option<String>,
    #[serde(default = "epoch")]
    pub evaluated_at: DateTime<Utc>,
}

impl Plan {
    /// Create a new `Plan` for the given file and phase.
    #[must_use]
    pub fn new(
        file: MediaFile,
        policy_name: impl Into<String>,
        phase_name: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            file,
            policy_name: policy_name.into(),
            phase_name: phase_name.into(),
            actions: Vec::new(),
            warnings: Vec::new(),
            safeguard_violations: Vec::new(),
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    #[must_use]
    pub fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    /// Set a skip reason and clear any existing actions.
    ///
    /// Skipped plans must have no actions, so this method enforces that invariant.
    #[must_use]
    pub fn with_skip_reason(mut self, reason: impl Into<String>) -> Self {
        self.skip_reason = Some(reason.into());
        self.actions.clear();
        self
    }

    #[must_use]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    #[must_use]
    pub fn with_action(mut self, action: PlannedAction) -> Self {
        self.actions.push(action);
        self
    }
}

/// A single action within a plan.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub operation: OperationType,
    pub track_index: Option<u32>,
    pub parameters: ActionParams,
    pub description: String,
}

impl PlannedAction {
    /// Create a new planned action for a file-level operation (no track index).
    #[must_use]
    pub fn file_op(
        operation: OperationType,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            track_index: None,
            parameters,
            description: description.into(),
        }
    }

    /// Create a new planned action targeting a specific track.
    #[must_use]
    pub fn track_op(
        operation: OperationType,
        track_index: u32,
        parameters: ActionParams,
        description: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            track_index: Some(track_index),
            parameters,
            description: description.into(),
        }
    }
}

/// Typed parameters for each operation type.
/// Replaces the previous untyped `serde_json::Value` parameters field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionParams {
    /// No parameters needed (SetDefault, ClearDefault, SetForced, ClearForced).
    Empty,
    /// Container conversion target.
    Container {
        container: Container,
    },
    /// Track removal with reason and track type.
    RemoveTrack {
        reason: String,
        track_type: TrackType,
    },
    /// Track reordering.
    ReorderTracks {
        order: Vec<String>,
    },
    /// Language assignment.
    Language {
        language: String,
    },
    /// Title assignment (empty string to clear).
    Title {
        title: String,
    },
    /// Transcode settings (codec, plus optional quality/encoding parameters).
    Transcode {
        codec: String,
        crf: Option<u32>,
        preset: Option<String>,
        bitrate: Option<String>,
        channels: Option<u32>,
    },
    /// Audio synthesis parameters.
    Synthesize {
        name: String,
        language: Option<String>,
        codec: Option<String>,
        text: Option<String>,
        bitrate: Option<String>,
        channels: Option<u32>,
        title: Option<String>,
        position: Option<String>,
        source_track: Option<u32>,
    },
    /// Container tag operations.
    SetTag {
        tag: String,
        value: String,
    },
    ClearTags {
        tags: Vec<String>,
    },
    DeleteTag {
        tag: String,
    },
}

/// The type of operation to perform on a media file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationType {
    SetDefault,
    ClearDefault,
    SetForced,
    ClearForced,
    SetTitle,
    SetLanguage,
    RemoveTrack,
    ReorderTracks,
    ConvertContainer,
    TranscodeVideo,
    TranscodeAudio,
    SynthesizeAudio,
    SetContainerTag,
    ClearContainerTags,
    DeleteContainerTag,
}

impl OperationType {
    /// Parse an `OperationType` from its canonical string representation.
    ///
    /// Returns `None` for unrecognised strings (e.g., from external WIT plugins using a
    /// newer schema version).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "set_default" => Some(Self::SetDefault),
            "clear_default" => Some(Self::ClearDefault),
            "set_forced" => Some(Self::SetForced),
            "clear_forced" => Some(Self::ClearForced),
            "set_title" => Some(Self::SetTitle),
            "set_language" => Some(Self::SetLanguage),
            "remove_track" => Some(Self::RemoveTrack),
            "reorder_tracks" => Some(Self::ReorderTracks),
            "convert_container" => Some(Self::ConvertContainer),
            "transcode_video" => Some(Self::TranscodeVideo),
            "transcode_audio" => Some(Self::TranscodeAudio),
            "synthesize_audio" => Some(Self::SynthesizeAudio),
            "set_container_tag" => Some(Self::SetContainerTag),
            "clear_container_tags" => Some(Self::ClearContainerTags),
            "delete_container_tag" => Some(Self::DeleteContainerTag),
            _ => None,
        }
    }

    /// The set of operation types that are metadata-only edits (no transcode or remux).
    ///
    /// Both the `FFmpeg` and `MKVToolNix` executors use this to decide whether a plan
    /// requires structural changes or can be handled via in-place metadata edits.
    pub const METADATA_OPS: &[OperationType] = &[
        OperationType::SetDefault,
        OperationType::ClearDefault,
        OperationType::SetForced,
        OperationType::ClearForced,
        OperationType::SetTitle,
        OperationType::SetLanguage,
        OperationType::SetContainerTag,
        OperationType::ClearContainerTags,
        OperationType::DeleteContainerTag,
    ];

    /// Returns `true` when this operation is a metadata-only edit (no transcode or remux).
    #[must_use]
    pub fn is_metadata_op(self) -> bool {
        Self::METADATA_OPS.contains(&self)
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            OperationType::SetDefault => "set_default",
            OperationType::ClearDefault => "clear_default",
            OperationType::SetForced => "set_forced",
            OperationType::ClearForced => "clear_forced",
            OperationType::SetTitle => "set_title",
            OperationType::SetLanguage => "set_language",
            OperationType::RemoveTrack => "remove_track",
            OperationType::ReorderTracks => "reorder_tracks",
            OperationType::ConvertContainer => "convert_container",
            OperationType::TranscodeVideo => "transcode_video",
            OperationType::TranscodeAudio => "transcode_audio",
            OperationType::SynthesizeAudio => "synthesize_audio",
            OperationType::SetContainerTag => "set_container_tag",
            OperationType::ClearContainerTags => "clear_container_tags",
            OperationType::DeleteContainerTag => "delete_container_tag",
        }
    }
}

/// The result of executing a single phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase_name: String,
    pub outcome: PhaseOutcome,
    pub actions: Vec<ActionResult>,
    pub file_modified: bool,
    pub skip_reason: Option<String>,
    pub duration_ms: u64,
}

impl PhaseResult {
    /// Create a new `PhaseResult` with the given phase name and outcome.
    #[must_use]
    pub fn new(phase_name: impl Into<String>, outcome: PhaseOutcome) -> Self {
        Self {
            phase_name: phase_name.into(),
            outcome,
            actions: Vec::new(),
            file_modified: false,
            skip_reason: None,
            duration_ms: 0,
        }
    }
}

/// The outcome of a phase execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseOutcome {
    Pending,
    Completed,
    Skipped,
    Failed,
}

/// The result of executing a single action within a phase.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub operation: OperationType,
    pub success: bool,
    pub description: String,
    pub error: Option<String>,
}

impl ActionResult {
    /// Create a successful `ActionResult`.
    #[must_use]
    pub fn success(operation: OperationType, description: impl Into<String>) -> Self {
        Self {
            operation,
            success: true,
            description: description.into(),
            error: None,
        }
    }

    /// Create a failed `ActionResult`.
    #[must_use]
    pub fn failure(
        operation: OperationType,
        description: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            success: false,
            description: description.into(),
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaFile;
    use std::path::PathBuf;

    fn sample_plan() -> Plan {
        Plan {
            id: Uuid::new_v4(),
            file: MediaFile::new(PathBuf::from("/test.mkv")),
            policy_name: "default".into(),
            phase_name: "normalize".into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: ActionParams::Empty,
                description: "Set track 1 as default".into(),
            }],
            warnings: vec![],
            safeguard_violations: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
        }
    }

    #[test]
    fn test_plan_is_empty() {
        let mut plan = sample_plan();
        assert!(!plan.is_empty());
        plan.actions.clear();
        assert!(plan.is_empty());
    }

    #[test]
    fn test_plan_is_skipped() {
        let mut plan = sample_plan();
        assert!(!plan.is_skipped());
        plan.skip_reason = Some("codec already correct".into());
        assert!(plan.is_skipped());
    }

    #[test]
    fn test_plan_serde_json_roundtrip() {
        let plan = sample_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let deserialized: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.policy_name, "default");
        assert_eq!(deserialized.actions.len(), 1);
        assert_eq!(deserialized.actions[0].operation, OperationType::SetDefault);
    }

    #[test]
    fn test_plan_serde_msgpack_roundtrip() {
        let plan = sample_plan();
        let bytes = rmp_serde::to_vec(&plan).unwrap();
        let deserialized: Plan = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.phase_name, "normalize");
        assert_eq!(deserialized.actions.len(), 1);
    }

    #[test]
    fn test_operation_type_as_str() {
        assert_eq!(OperationType::SetDefault.as_str(), "set_default");
        assert_eq!(OperationType::TranscodeVideo.as_str(), "transcode_video");
    }

    #[test]
    fn test_plan_builder_methods() {
        let plan = sample_plan();
        let action = PlannedAction {
            operation: OperationType::RemoveTrack,
            track_index: Some(2),
            parameters: ActionParams::RemoveTrack {
                reason: "test".into(),
                track_type: TrackType::AudioMain,
            },
            description: "Remove track 2".into(),
        };

        let plan = plan.with_warning("test warning").with_action(action);

        assert_eq!(plan.warnings, vec!["test warning"]);
        assert_eq!(plan.actions.len(), 2);

        // with_skip_reason clears actions to avoid inconsistent state
        let plan = plan.with_skip_reason("no changes needed");
        assert!(plan.is_skipped());
        assert_eq!(plan.skip_reason.as_deref(), Some("no changes needed"));
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn test_plan_serde_backward_compat() {
        // JSON without id/policy_hash/evaluated_at should deserialize with defaults
        let json = r#"{
            "file": {"id":"00000000-0000-0000-0000-000000000000","path":"/test.mkv","size":0,"content_hash":"","container":"Other","duration":0.0,"bitrate":null,"tracks":[],"tags":{},"plugin_metadata":{},"introspected_at":"2024-01-01T00:00:00Z"},
            "policy_name": "test",
            "phase_name": "init",
            "actions": [],
            "warnings": [],
            "skip_reason": null
        }"#;
        let plan: Plan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.policy_name, "test");
        assert!(plan.policy_hash.is_none());
    }
}
