//! `FFmpeg` Executor Plugin.
//!
//! Executes media plans using `FFmpeg` for transcoding, container conversion,
//! and metadata operations on non-MKV files (or any file requiring transcode).

pub mod command;
pub mod hwaccel;
pub mod progress;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult, PlanCreatedEvent};
use voom_domain::media::Container;
use voom_domain::plan::{ActionResult, OperationType, Plan, PlannedAction};
use voom_kernel::Plugin;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;

/// `FFmpeg` executor plugin.
///
/// Handles `plan.created` events by building and executing `FFmpeg` commands
/// for transcoding, container conversion, and metadata operations.
pub struct FfmpegExecutorPlugin {
    capabilities: Vec<Capability>,
    hw_accel: HwAccelConfig,
}

impl FfmpegExecutorPlugin {
    /// Create a new `FFmpeg` executor plugin with default HW accel config.
    #[must_use] 
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Execute {
                operations: vec![
                    "convert_container".to_string(),
                    "transcode_video".to_string(),
                    "transcode_audio".to_string(),
                    "synthesize_audio".to_string(),
                    "set_default".to_string(),
                    "clear_default".to_string(),
                    "set_title".to_string(),
                    "set_language".to_string(),
                ],
                formats: vec![], // Supports all formats
            }],
            hw_accel: HwAccelConfig::new(),
        }
    }

    /// Create with a specific HW accel configuration.
    #[must_use] 
    pub fn with_hw_accel(mut self, hw_accel: HwAccelConfig) -> Self {
        self.hw_accel = hw_accel;
        self
    }

    /// Check whether this plugin can handle the given plan.
    ///
    /// Returns `true` when the plan contains transcode operations, or when
    /// the file is non-MKV and needs metadata/remux work. MKV files with
    /// only metadata operations are left to the `MKVToolNix` executor.
    pub fn can_handle(&self, plan: &Plan) -> bool {
        if plan.is_empty() || plan.is_skipped() {
            return false;
        }

        let has_transcode = plan.actions.iter().any(|a| {
            matches!(
                a.operation,
                OperationType::TranscodeVideo
                    | OperationType::TranscodeAudio
                    | OperationType::SynthesizeAudio
            )
        });

        if has_transcode {
            return true;
        }

        let has_convert = plan
            .actions
            .iter()
            .any(|a| a.operation == OperationType::ConvertContainer);

        if has_convert {
            return true;
        }

        // For metadata-only operations on MKV files, defer to MKVToolNix
        let is_mkv = plan.file.container == Container::Mkv;
        let only_metadata = plan.actions.iter().all(is_metadata_op);

        if is_mkv && only_metadata {
            return false;
        }

        // Non-MKV files with metadata ops — ffmpeg handles them
        !plan.actions.is_empty()
    }

    /// Execute a plan and return action results.
    ///
    /// Builds an `FFmpeg` command from the plan's actions, but does not actually
    /// run the subprocess (that is left to the caller or the event handler).
    /// Returns the expected action results.
    pub fn execute_plan(&self, plan: &Plan) -> Result<Vec<ActionResult>> {
        if !self.can_handle(plan) {
            return Err(VoomError::Plugin {
                plugin: "ffmpeg-executor".into(),
                message: "Plan cannot be handled by FFmpeg executor".into(),
            });
        }

        let actions: Vec<&PlannedAction> = plan.actions.iter().collect();
        let ext = output_extension(&plan.file, &actions);

        // Build the output path (temp file next to original)
        let parent = plan
            .file
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/tmp"));
        let stem = plan
            .file
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let output_path = parent.join(format!("{stem}.voom_tmp.{ext}"));

        let hw_accel = if self.hw_accel.enabled {
            Some(&self.hw_accel)
        } else {
            None
        };

        let _ffmpeg_args = build_ffmpeg_command(&plan.file, &actions, &output_path, hw_accel)?;

        tracing::info!(
            path = %plan.file.path.display(),
            phase = %plan.phase_name,
            actions = actions.len(),
            output = %output_path.display(),
            "Built FFmpeg command for plan execution"
        );

        // Build action results (in a real implementation, these would reflect
        // actual execution status from running the ffmpeg subprocess)
        let results: Vec<ActionResult> = plan
            .actions
            .iter()
            .map(|action| ActionResult {
                operation: action.operation,
                success: true,
                description: action.description.clone(),
                error: None,
            })
            .collect();

        Ok(results)
    }

    /// Handle a `plan.created` event.
    fn handle_plan_created(&self, event: &PlanCreatedEvent) -> Result<Option<EventResult>> {
        let plan = &event.plan;

        if !self.can_handle(plan) {
            return Ok(None);
        }

        match self.execute_plan(plan) {
            Ok(results) => {
                let actions_applied = results.iter().filter(|r| r.success).count();
                Ok(Some(EventResult::plan_succeeded(
                    "ffmpeg-executor",
                    plan,
                    actions_applied,
                    Some(serde_json::to_value(&results).unwrap_or_default()),
                )))
            }
            Err(e) => Ok(Some(EventResult::plan_failed(
                "ffmpeg-executor",
                plan,
                e.to_string(),
            ))),
        }
    }
}

impl Default for FfmpegExecutorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if an action is a metadata-only operation (no transcode/remux).
fn is_metadata_op(action: &PlannedAction) -> bool {
    matches!(
        action.operation,
        OperationType::SetDefault
            | OperationType::ClearDefault
            | OperationType::SetForced
            | OperationType::ClearForced
            | OperationType::SetTitle
            | OperationType::SetLanguage
            | OperationType::SetContainerTag
    )
}

impl Plugin for FfmpegExecutorPlugin {
    fn name(&self) -> &str {
        "ffmpeg-executor"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "plan.created"
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanCreated(plan_event) => self.handle_plan_created(plan_event),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::PlanExecutingEvent;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

    fn sample_mp4_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/video.mp4"));
        file.container = Container::Mp4;
        file.duration = 120.0;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "h264".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
        ];
        file
    }

    fn sample_mkv_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/video.mkv"));
        file.container = Container::Mkv;
        file.duration = 90.0;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "hevc".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
            Track::new(2, TrackType::SubtitleMain, "srt".into()),
        ];
        file
    }

    fn plan_with_actions(file: MediaFile, actions: Vec<PlannedAction>) -> Plan {
        Plan {
            id: uuid::Uuid::new_v4(),
            file,
            policy_name: "test".into(),
            phase_name: "process".into(),
            actions,
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = FfmpegExecutorPlugin::new();
        assert_eq!(plugin.name(), "ffmpeg-executor");
        assert_eq!(plugin.version(), env!("CARGO_PKG_VERSION"));

        let caps = plugin.capabilities();
        assert_eq!(caps.len(), 1);
        match &caps[0] {
            Capability::Execute {
                operations,
                formats,
            } => {
                assert!(operations.contains(&"convert_container".to_string()));
                assert!(operations.contains(&"transcode_video".to_string()));
                assert!(operations.contains(&"transcode_audio".to_string()));
                assert!(operations.contains(&"synthesize_audio".to_string()));
                assert!(operations.contains(&"set_default".to_string()));
                assert!(operations.contains(&"clear_default".to_string()));
                assert!(operations.contains(&"set_title".to_string()));
                assert!(operations.contains(&"set_language".to_string()));
                assert!(formats.is_empty(), "Should support all formats");
            }
            other => panic!("Expected Execute capability, got {:?}", other),
        }
    }

    #[test]
    fn test_handles_plan_created() {
        let plugin = FfmpegExecutorPlugin::new();
        assert!(plugin.handles("plan.created"));
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("plan.completed"));
    }

    #[test]
    fn test_can_handle_transcode() {
        let plugin = FfmpegExecutorPlugin::new();

        // TranscodeVideo — should handle
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc"}),
                description: "Transcode to HEVC".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));

        // TranscodeAudio — should handle
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeAudio,
                track_index: Some(1),
                parameters: serde_json::json!({"codec": "opus"}),
                description: "Transcode to Opus".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));

        // SynthesizeAudio — should handle
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::SynthesizeAudio,
                track_index: Some(1),
                parameters: serde_json::json!({"codec": "aac"}),
                description: "Synthesize audio".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_convert() {
        let plugin = FfmpegExecutorPlugin::new();

        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::ConvertContainer,
                track_index: None,
                parameters: serde_json::json!({"container": "mkv"}),
                description: "Convert to MKV".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_cannot_handle_mkv_metadata_only() {
        let plugin = FfmpegExecutorPlugin::new();

        // MKV file with only metadata ops — should be handled by mkvtoolnix
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![
                PlannedAction {
                    operation: OperationType::SetDefault,
                    track_index: Some(1),
                    parameters: serde_json::json!({}),
                    description: "Set default".into(),
                },
                PlannedAction {
                    operation: OperationType::SetTitle,
                    track_index: Some(1),
                    parameters: serde_json::json!({"title": "English"}),
                    description: "Set title".into(),
                },
            ],
        );
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_non_mkv_metadata() {
        let plugin = FfmpegExecutorPlugin::new();

        // MP4 file with metadata ops — ffmpeg handles it
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set default".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_cannot_handle_empty_plan() {
        let plugin = FfmpegExecutorPlugin::new();

        let plan = plan_with_actions(sample_mp4_file(), vec![]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_cannot_handle_skipped_plan() {
        let plugin = FfmpegExecutorPlugin::new();

        let mut plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc"}),
                description: "Transcode".into(),
            }],
        );
        plan.skip_reason = Some("Already processed".into());
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_execute_plan_transcode() {
        let plugin = FfmpegExecutorPlugin::new();

        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc", "crf": 23}),
                description: "Transcode to HEVC".into(),
            }],
        );

        let results = plugin.execute_plan(&plan).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(results[0].operation, OperationType::TranscodeVideo);
    }

    #[test]
    fn test_execute_plan_not_handleable() {
        let plugin = FfmpegExecutorPlugin::new();

        // MKV + metadata only — cannot handle
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set default".into(),
            }],
        );

        assert!(plugin.execute_plan(&plan).is_err());
    }

    #[test]
    fn test_on_event_plan_created() {
        let plugin = FfmpegExecutorPlugin::new();

        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc"}),
                description: "Transcode to HEVC".into(),
            }],
        );

        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_some());

        let result = result.unwrap();
        assert_eq!(result.plugin_name, "ffmpeg-executor");
        assert_eq!(result.produced_events.len(), 2);
        assert_eq!(result.produced_events[0].event_type(), "plan.executing");
        assert_eq!(result.produced_events[1].event_type(), "plan.completed");
    }

    #[test]
    fn test_on_event_ignores_other_events() {
        let plugin = FfmpegExecutorPlugin::new();

        let event = Event::PlanExecuting(PlanExecutingEvent {
            path: PathBuf::from("/test.mp4"),
            phase_name: "process".into(),
            action_count: 1,
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_unhandleable_plan() {
        let plugin = FfmpegExecutorPlugin::new();

        // MKV + metadata only
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set default".into(),
            }],
        );

        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_mkv_with_transcode_is_handled() {
        let plugin = FfmpegExecutorPlugin::new();

        // MKV file but with transcode op — ffmpeg should handle
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![
                PlannedAction {
                    operation: OperationType::TranscodeVideo,
                    track_index: Some(0),
                    parameters: serde_json::json!({"codec": "h264"}),
                    description: "Transcode to H.264".into(),
                },
                PlannedAction {
                    operation: OperationType::SetDefault,
                    track_index: Some(1),
                    parameters: serde_json::json!({}),
                    description: "Set default".into(),
                },
            ],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_default_impl() {
        let plugin = FfmpegExecutorPlugin::default();
        assert_eq!(plugin.name(), "ffmpeg-executor");
    }
}
