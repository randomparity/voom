use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::MediaFile;

fn epoch() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH
}

/// A plan produced by the policy evaluator for a single file in a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    pub file: MediaFile,
    pub policy_name: String,
    pub phase_name: String,
    pub actions: Vec<PlannedAction>,
    pub warnings: Vec<String>,
    pub skip_reason: Option<String>,
    #[serde(default)]
    pub policy_hash: Option<String>,
    #[serde(default = "epoch")]
    pub evaluated_at: DateTime<Utc>,
}

impl Plan {
    /// Returns true if this plan has no actions to execute.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Returns true if this plan was skipped.
    #[must_use]
    pub fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }

    /// Returns a new Plan with the given skip reason set.
    #[must_use]
    pub fn with_skip_reason(mut self, reason: impl Into<String>) -> Self {
        self.skip_reason = Some(reason.into());
        self.actions.clear();
        self
    }

    /// Returns a new Plan with an additional warning.
    #[must_use]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Returns a new Plan with an additional action.
    #[must_use]
    pub fn with_action(mut self, action: PlannedAction) -> Self {
        self.actions.push(action);
        self
    }
}

/// A single action within a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedAction {
    pub operation: OperationType,
    pub track_index: Option<u32>,
    pub parameters: serde_json::Value,
    pub description: String,
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
    /// The set of operation types that are metadata-only edits (no transcode or remux).
    ///
    /// Both the FFmpeg and MKVToolNix executors use this to decide whether a plan
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase_name: String,
    pub outcome: PhaseOutcome,
    pub actions: Vec<ActionResult>,
    pub file_modified: bool,
    pub skip_reason: Option<String>,
    pub duration_ms: u64,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub operation: OperationType,
    pub success: bool,
    pub description: String,
    pub error: Option<String>,
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
                parameters: serde_json::json!({}),
                description: "Set track 1 as default".into(),
            }],
            warnings: vec![],
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
            parameters: serde_json::json!({}),
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
