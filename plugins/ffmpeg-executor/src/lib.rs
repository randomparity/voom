//! `FFmpeg` Executor Plugin.
//!
//! Executes media plans using `FFmpeg` for transcoding, container conversion,
//! and metadata operations on non-MKV files (or any file requiring transcode).

#![allow(clippy::missing_errors_doc)]

pub mod command;
pub mod hwaccel;
pub mod progress;

use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult, PlanCreatedEvent};
use voom_domain::media::Container;
use voom_domain::plan::{ActionResult, OperationType, Plan, PlannedAction};
use voom_kernel::Plugin;
use wait_timeout::ChildExt;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;

/// Default timeout for FFmpeg operations (4 hours — transcode can be slow).
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Operations that FFmpeg handles: transcode/synthesize, container conversion,
/// and metadata edits on non-MKV files.
///
/// This list declares the *capability* the plugin advertises. `can_handle()`
/// enforces the actual dispatch rules at runtime.
const FFMPEG_OPS: &[OperationType] = &[
    OperationType::ConvertContainer,
    OperationType::TranscodeVideo,
    OperationType::TranscodeAudio,
    OperationType::SynthesizeAudio,
    // Metadata ops — handled by FFmpeg on non-MKV files
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

/// Drain stdout and stderr pipes from a child process into buffers.
///
/// **Precondition**: The child process must have exited or been killed before
/// calling this. Calling it on a live process will deadlock if either pipe
/// fills its OS buffer.
fn drain_pipes(child: &mut std::process::Child) -> (Vec<u8>, Vec<u8>) {
    use std::io::Read;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout_buf).ok();
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_end(&mut stderr_buf).ok();
    }
    (stdout_buf, stderr_buf)
}

/// Run a subprocess with a timeout, killing it if it exceeds the deadline.
fn run_with_timeout(tool: &str, args: &[impl AsRef<OsStr>], timeout: Duration) -> Result<Output> {
    let mut child = Command::new(tool)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| VoomError::ToolExecution {
            tool: tool.into(),
            message: format!("failed to spawn {tool}: {e}"),
        })?;

    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            let (stdout, stderr) = drain_pipes(&mut child);
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        }
        Ok(None) => {
            child.kill().ok();
            drain_pipes(&mut child);
            child.wait().ok();
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("{tool} timed out after {}s", timeout.as_secs()),
            })
        }
        Err(e) => {
            child.kill().ok();
            drain_pipes(&mut child);
            child.wait().ok();
            Err(VoomError::ToolExecution {
                tool: tool.into(),
                message: format!("error waiting for {tool}: {e}"),
            })
        }
    }
}

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
                operations: FFMPEG_OPS.to_vec(),
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
    /// Returns `true` for:
    /// - Plans containing transcode, synthesize, or container conversion ops
    /// - Non-MKV files with metadata-only operations
    ///
    /// Returns `false` for:
    /// - Empty or skipped plans
    /// - MKV files with only metadata operations (deferred to mkvtoolnix)
    #[must_use]
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
        let has_convert = plan
            .actions
            .iter()
            .any(|a| a.operation == OperationType::ConvertContainer);

        // FFmpeg always handles transcode/synthesize/convert-container
        if has_transcode || has_convert {
            return true;
        }

        // Metadata-only ops: FFmpeg handles non-MKV files.
        // MKV metadata stays with mkvtoolnix (faster for in-place edits).
        let is_mkv = plan.file.container == Container::Mkv;
        if !is_mkv && plan.actions.iter().all(|a| a.operation.is_metadata_op()) {
            return true;
        }

        false
    }

    /// Execute a plan by spawning an FFmpeg subprocess.
    ///
    /// Builds FFmpeg args, runs the command writing to a temp file, then
    /// renames the temp file over the original (or to the new extension
    /// if converting containers).
    pub fn execute_plan(&self, plan: &Plan) -> Result<Vec<ActionResult>> {
        if !self.can_handle(plan) {
            return Err(VoomError::Plugin {
                plugin: "ffmpeg-executor".into(),
                message: "Plan cannot be handled by FFmpeg executor".into(),
            });
        }

        if !plan.file.path.exists() {
            return Err(VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!("file not found: {}", plan.file.path.display()),
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
        let output_path = parent.join(format!("{stem}.voom_tmp_{}.{ext}", plan.id));

        let hw_accel = self.hw_accel.enabled().then_some(&self.hw_accel);
        let ffmpeg_args = build_ffmpeg_command(&plan.file, &actions, &output_path, hw_accel)?;

        tracing::info!(
            path = %plan.file.path.display(),
            phase = %plan.phase_name,
            actions = actions.len(),
            output = %output_path.display(),
            "executing ffmpeg"
        );

        let output = run_with_timeout("ffmpeg", &ffmpeg_args, FFMPEG_TIMEOUT);

        match output {
            Ok(output) if output.status.success() => {
                // Determine final path: if container changed, use new extension
                let final_path = if ext
                    != plan
                        .file
                        .path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                {
                    // Container conversion: rename to new extension
                    let new_path = plan.file.path.with_extension(&ext);
                    std::fs::rename(&output_path, &new_path).map_err(|e| {
                        let _ = std::fs::remove_file(&output_path);
                        VoomError::ToolExecution {
                            tool: "ffmpeg".into(),
                            message: format!(
                                "failed to rename temp file to {}: {e}",
                                new_path.display()
                            ),
                        }
                    })?;
                    // Remove old file if extension changed
                    if new_path != plan.file.path {
                        let _ = std::fs::remove_file(&plan.file.path);
                    }
                    new_path
                } else {
                    // Same extension: rename temp over original
                    std::fs::rename(&output_path, &plan.file.path).map_err(|e| {
                        let _ = std::fs::remove_file(&output_path);
                        VoomError::ToolExecution {
                            tool: "ffmpeg".into(),
                            message: format!(
                                "failed to rename temp file to {}: {e}",
                                plan.file.path.display()
                            ),
                        }
                    })?;
                    plan.file.path.clone()
                };

                tracing::info!(
                    path = %final_path.display(),
                    actions = actions.len(),
                    "ffmpeg execution complete"
                );

                Ok(actions
                    .iter()
                    .map(|a| ActionResult {
                        operation: a.operation,
                        success: true,
                        description: a.description.clone(),
                        error: None,
                    })
                    .collect())
            }
            Ok(output) => {
                let _ = std::fs::remove_file(&output_path);
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(VoomError::ToolExecution {
                    tool: "ffmpeg".into(),
                    message: format!(
                        "ffmpeg exited with {}: {}",
                        output.status,
                        stderr.lines().last().unwrap_or("(no output)")
                    ),
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&output_path);
                Err(e)
            }
        }
    }

    /// Handle a `plan.created` event.
    fn handle_plan_created(&self, event: &PlanCreatedEvent) -> Result<Option<EventResult>> {
        let plan = &event.plan;

        if plan.is_empty() || plan.is_skipped() {
            return Ok(None);
        }

        if !self.can_handle(plan) {
            tracing::debug!(
                path = %plan.file.path.display(),
                phase = %plan.phase_name,
                "plan not handled by ffmpeg executor"
            );
            return Ok(None);
        }

        Ok(Some(EventResult::from_plan_execution(
            "ffmpeg-executor",
            self.execute_plan(plan),
        )))
    }
}

impl Default for FfmpegExecutorPlugin {
    fn default() -> Self {
        Self::new()
    }
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
        event_type == Event::PLAN_CREATED
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
    use voom_domain::plan::ActionParams;

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
                assert!(operations.contains(&OperationType::ConvertContainer));
                assert!(operations.contains(&OperationType::TranscodeVideo));
                assert!(operations.contains(&OperationType::TranscodeAudio));
                assert!(operations.contains(&OperationType::SynthesizeAudio));
                assert!(operations.contains(&OperationType::SetDefault));
                assert!(operations.contains(&OperationType::ClearDefault));
                assert!(operations.contains(&OperationType::SetTitle));
                assert!(operations.contains(&OperationType::SetLanguage));
                assert!(formats.is_empty(), "Should support all formats");
            }
            other => panic!("Expected Execute capability, got {:?}", other),
        }
    }

    #[test]
    fn test_handles_plan_created() {
        let plugin = FfmpegExecutorPlugin::new();
        assert!(plugin.handles(Event::PLAN_CREATED));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_COMPLETED));
    }

    // ── can_handle: positive cases ──────────────────────────────

    #[test]
    fn test_can_handle_transcode_video() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode to HEVC".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_transcode_audio() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeAudio,
                track_index: Some(1),
                parameters: ActionParams::Transcode {
                    codec: "opus".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode to Opus".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_synthesize_audio() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::SynthesizeAudio,
                track_index: Some(1),
                parameters: ActionParams::Synthesize {
                    name: "stereo".into(),
                    codec: Some("aac".into()),
                    language: None,
                    text: None,
                    bitrate: None,
                    channels: None,
                    title: None,
                    position: None,
                    source_track: None,
                },
                description: "Synthesize audio".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_convert_container() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::ConvertContainer,
                track_index: None,
                parameters: ActionParams::Container {
                    container: Container::Mkv,
                },
                description: "Convert to MKV".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_non_mkv_metadata() {
        let plugin = FfmpegExecutorPlugin::new();
        // MP4 file with metadata ops — FFmpeg handles non-MKV metadata
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: ActionParams::Empty,
                description: "Set default".into(),
            }],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_mkv_with_transcode() {
        let plugin = FfmpegExecutorPlugin::new();
        // MKV file with transcode — FFmpeg handles all transcodes regardless of container
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![
                PlannedAction {
                    operation: OperationType::TranscodeVideo,
                    track_index: Some(0),
                    parameters: ActionParams::Transcode {
                        codec: "h264".into(),
                        crf: None,
                        preset: None,
                        bitrate: None,
                        channels: None,
                    },
                    description: "Transcode to H.264".into(),
                },
                PlannedAction {
                    operation: OperationType::SetDefault,
                    track_index: Some(1),
                    parameters: ActionParams::Empty,
                    description: "Set default".into(),
                },
            ],
        );
        assert!(plugin.can_handle(&plan));
    }

    // ── can_handle: negative cases ──────────────────────────────

    #[test]
    fn test_cannot_handle_mkv_metadata_only() {
        let plugin = FfmpegExecutorPlugin::new();
        // MKV file with only metadata ops — mkvtoolnix handles these
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![
                PlannedAction {
                    operation: OperationType::SetDefault,
                    track_index: Some(1),
                    parameters: ActionParams::Empty,
                    description: "Set default".into(),
                },
                PlannedAction {
                    operation: OperationType::SetTitle,
                    track_index: Some(1),
                    parameters: ActionParams::Title {
                        title: "English".into(),
                    },
                    description: "Set title".into(),
                },
            ],
        );
        assert!(!plugin.can_handle(&plan));
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
                parameters: ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode".into(),
            }],
        );
        plan.skip_reason = Some("Already processed".into());
        assert!(!plugin.can_handle(&plan));
    }

    // ── execute_plan ─────────────────────────────────────────────

    #[test]
    fn test_execute_plan_not_handleable() {
        let plugin = FfmpegExecutorPlugin::new();
        // MKV + metadata only — cannot handle
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: ActionParams::Empty,
                description: "Set default".into(),
            }],
        );
        assert!(plugin.execute_plan(&plan).is_err());
    }

    #[test]
    fn test_execute_plan_file_not_found() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(), // /media/video.mp4 does not exist
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: Some(23),
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode to HEVC".into(),
            }],
        );

        let result = plugin.execute_plan(&plan);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("file not found"), "got: {err}");
    }

    // ── on_event dispatch ─────────────────────────────────────────

    #[test]
    fn test_on_event_claims_transcode_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode to HEVC".into(),
            }],
        );

        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        // Plan is claimed (file doesn't exist so execution fails, but it IS claimed)
        assert!(result.is_some(), "plugin should claim transcode plans");
        assert!(result.unwrap().claimed);
    }

    #[test]
    fn test_on_event_skips_mkv_metadata_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: ActionParams::Empty,
                description: "Set default".into(),
            }],
        );

        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
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
    fn test_on_event_skips_empty_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(sample_mp4_file(), vec![]);
        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_skipped_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let mut plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                description: "Transcode".into(),
            }],
        );
        plan.skip_reason = Some("Already processed".into());
        let event = Event::PlanCreated(PlanCreatedEvent { plan });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_default_impl() {
        let plugin = FfmpegExecutorPlugin::default();
        assert_eq!(plugin.name(), "ffmpeg-executor");
    }
}
