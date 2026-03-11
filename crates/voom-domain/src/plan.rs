use serde::{Deserialize, Serialize};

use crate::media::MediaFile;

/// A plan produced by the policy evaluator for a single file in a single phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub file: MediaFile,
    pub policy_name: String,
    pub phase_name: String,
    pub actions: Vec<PlannedAction>,
    pub warnings: Vec<String>,
    pub skip_reason: Option<String>,
}

impl Plan {
    /// Returns true if this plan has no actions to execute.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Returns true if this plan was skipped.
    pub fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
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
}

impl OperationType {
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
}
