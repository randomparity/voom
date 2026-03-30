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
    /// Hint indicating which executor plugin should handle this plan,
    /// set by capability-aware validation when a single executor matches.
    #[serde(default)]
    pub executor_hint: Option<String>,
}

impl Plan {
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
            executor_hint: None,
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

/// Channel setting for a transcode action — either a named preset or a count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TranscodeChannels {
    /// Named preset, e.g. `stereo`, `preserve`.
    Named(String),
    /// Explicit channel count, e.g. `2`, `6`.
    Count(u32),
}

impl TranscodeChannels {
    /// Resolve to a concrete channel count.
    /// Returns `None` for "preserve" or unrecognized named presets.
    #[must_use]
    pub fn to_count(&self) -> Option<u32> {
        match self {
            Self::Count(n) => Some(*n),
            Self::Named(name) => match name.as_str() {
                "mono" => Some(1),
                "stereo" => Some(2),
                "5.1" | "surround" => Some(6),
                "7.1" => Some(8),
                _ => None,
            },
        }
    }
}

/// Transcode quality/encoding settings, separate from the codec choice.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TranscodeSettings {
    pub crf: Option<u32>,
    pub preset: Option<String>,
    pub bitrate: Option<String>,
    pub channels: Option<TranscodeChannels>,
    /// Per-action HW acceleration preference (overrides system-wide detection).
    pub hw: Option<String>,
    /// Whether to fall back to software encoding when HW is unavailable.
    pub hw_fallback: Option<bool>,
    /// Maximum resolution (e.g. "1080p"). Downscale if source exceeds.
    pub max_resolution: Option<String>,
    /// Scaling algorithm (e.g. "lanczos").
    pub scale_algorithm: Option<String>,
    /// HDR handling mode (e.g. "preserve", "tonemap").
    pub hdr_mode: Option<String>,
    /// Encoder tuning hint (e.g. "film", "animation").
    pub tune: Option<String>,
}

impl TranscodeSettings {
    #[must_use]
    pub fn with_crf(mut self, crf: Option<u32>) -> Self {
        self.crf = crf;
        self
    }

    #[must_use]
    pub fn with_preset(mut self, preset: Option<String>) -> Self {
        self.preset = preset;
        self
    }

    #[must_use]
    pub fn with_bitrate(mut self, bitrate: Option<String>) -> Self {
        self.bitrate = bitrate;
        self
    }

    #[must_use]
    pub fn with_channels(mut self, channels: Option<TranscodeChannels>) -> Self {
        self.channels = channels;
        self
    }

    #[must_use]
    pub fn with_hw(mut self, hw: Option<String>) -> Self {
        self.hw = hw;
        self
    }

    #[must_use]
    pub fn with_hw_fallback(mut self, hw_fallback: Option<bool>) -> Self {
        self.hw_fallback = hw_fallback;
        self
    }

    #[must_use]
    pub fn with_max_resolution(mut self, max_resolution: Option<String>) -> Self {
        self.max_resolution = max_resolution;
        self
    }

    #[must_use]
    pub fn with_scale_algorithm(mut self, scale_algorithm: Option<String>) -> Self {
        self.scale_algorithm = scale_algorithm;
        self
    }

    #[must_use]
    pub fn with_hdr_mode(mut self, hdr_mode: Option<String>) -> Self {
        self.hdr_mode = hdr_mode;
        self
    }

    #[must_use]
    pub fn with_tune(mut self, tune: Option<String>) -> Self {
        self.tune = tune;
        self
    }
}

/// Typed parameters for each operation type.
/// Replaces the previous untyped `serde_json::Value` parameters field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionParams {
    /// No parameters needed (`SetDefault`, `ClearDefault`, `SetForced`, `ClearForced`).
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
        #[serde(default)]
        settings: TranscodeSettings,
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
    /// Set a track's title. An empty title string means "clear/remove the title".
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
    #[must_use]
    pub fn success(operation: OperationType, description: impl Into<String>) -> Self {
        Self {
            operation,
            success: true,
            description: description.into(),
            error: None,
        }
    }

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
            executor_hint: None,
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
    fn test_transcode_channels_to_count_named() {
        assert_eq!(TranscodeChannels::Named("mono".into()).to_count(), Some(1));
        assert_eq!(
            TranscodeChannels::Named("stereo".into()).to_count(),
            Some(2)
        );
        assert_eq!(TranscodeChannels::Named("5.1".into()).to_count(), Some(6));
        assert_eq!(
            TranscodeChannels::Named("surround".into()).to_count(),
            Some(6)
        );
        assert_eq!(TranscodeChannels::Named("7.1".into()).to_count(), Some(8));
    }

    #[test]
    fn test_transcode_channels_to_count_numeric() {
        assert_eq!(TranscodeChannels::Count(1).to_count(), Some(1));
        assert_eq!(TranscodeChannels::Count(2).to_count(), Some(2));
        assert_eq!(TranscodeChannels::Count(6).to_count(), Some(6));
    }

    #[test]
    fn test_transcode_channels_to_count_preserve_returns_none() {
        assert_eq!(TranscodeChannels::Named("preserve".into()).to_count(), None);
    }

    #[test]
    fn test_transcode_channels_to_count_unknown_returns_none() {
        assert_eq!(
            TranscodeChannels::Named("quadraphonic".into()).to_count(),
            None
        );
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
