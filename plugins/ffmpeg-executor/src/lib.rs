//! `FFmpeg` Executor Plugin.
//!
//! Executes media plans using `FFmpeg` for transcoding, container conversion,
//! and metadata operations on non-MKV files (or any file requiring transcode).

pub mod command;
pub mod hwaccel;
pub mod progress;

use std::process::Command;
use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{
    CodecCapabilities, Event, EventResult, ExecutorCapabilitiesEvent, PlanCreatedEvent,
};

fn plugin_err(message: impl Into<String>) -> VoomError {
    VoomError::plugin("ffmpeg-executor", message)
}
use voom_domain::media::Container;
use voom_domain::plan::{ActionResult, OperationType, Plan, PlannedAction};
use voom_kernel::{Plugin, PluginContext};
use voom_process::run_with_timeout;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;

/// Default timeout for `FFmpeg` operations (4 hours — transcode can be slow).
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Operations that `FFmpeg` handles: transcode/synthesize, container conversion,
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

/// `FFmpeg` executor plugin.
///
/// Handles `plan.created` events by building and executing `FFmpeg` commands
/// for transcoding, container conversion, and metadata operations.
pub struct FfmpegExecutorPlugin {
    capabilities: Vec<Capability>,
    hw_accel: HwAccelConfig,
    probed_codecs: Option<CodecCapabilities>,
    probed_formats: Option<Vec<String>>,
    probed_hw_accels: Option<Vec<String>>,
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
            probed_codecs: None,
            probed_formats: None,
            probed_hw_accels: None,
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

    /// Execute a plan by spawning an `FFmpeg` subprocess.
    ///
    /// Builds `FFmpeg` args, runs the command writing to a temp file, then
    /// renames the temp file over the original (or to the new extension
    /// if converting containers).
    pub fn execute_plan(&self, plan: &Plan) -> Result<Vec<ActionResult>> {
        if !self.can_handle(plan) {
            return Err(plugin_err("Plan cannot be handled by FFmpeg executor"));
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
                    .map(|a| ActionResult::success(a.operation, a.description.clone()))
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

/// Parse `ffmpeg -codecs` output into decoder and encoder lists.
///
/// Each codec line (after the `-------` separator) has flags in columns 0-5:
/// `D` = decoding, `E` = encoding. The codec name follows after whitespace.
fn parse_codecs(output: &str) -> CodecCapabilities {
    let mut decoders = Vec::new();
    let mut encoders = Vec::new();
    let mut past_separator = false;

    for line in output.lines() {
        if line.starts_with(" ------") {
            past_separator = true;
            continue;
        }
        if !past_separator {
            continue;
        }
        // Lines look like: " DEV.L. h264   H.264 / AVC / MPEG-4 AVC"
        // Flags are in columns 1-6, codec name starts after whitespace
        let trimmed = line.trim_start();
        if trimmed.len() < 8 {
            continue;
        }
        let flags = &trimmed[..6];
        let rest = trimmed[6..].trim_start();
        let name = rest.split_whitespace().next().unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        if flags.starts_with('D') {
            decoders.push(name.clone());
        }
        if flags.chars().nth(1) == Some('E') {
            encoders.push(name);
        }
    }

    CodecCapabilities::new(decoders, encoders)
}

/// Parse `ffmpeg -formats` output into a list of supported format names.
///
/// Each format line (after the `-------` separator) has flags in columns 0-2:
/// `D` = demux, `E` = mux. We collect any format that can be muxed or demuxed.
fn parse_formats(output: &str) -> Vec<String> {
    let mut formats = Vec::new();
    let mut past_separator = false;

    for line in output.lines() {
        if line.starts_with(" ------") {
            past_separator = true;
            continue;
        }
        if !past_separator {
            continue;
        }
        // Lines look like: " DE matroska,webm Matroska / WebM"
        let trimmed = line.trim_start();
        if trimmed.len() < 4 {
            continue;
        }
        let rest = trimmed[2..].trim_start();
        let name_field = rest.split_whitespace().next().unwrap_or("");
        // Some formats list aliases: "matroska,webm" — take the primary
        for name in name_field.split(',') {
            let name = name.trim();
            if !name.is_empty() {
                formats.push(name.to_string());
            }
        }
    }

    formats.sort();
    formats.dedup();
    formats
}

/// Parse `ffmpeg -hwaccels` output into a list of hardware acceleration names.
///
/// Lines after "Hardware acceleration methods:" are individual backend names.
fn parse_hwaccels(output: &str) -> Vec<String> {
    let mut accels = Vec::new();
    let mut past_header = false;

    for line in output.lines() {
        if line.contains("Hardware acceleration methods:") {
            past_header = true;
            continue;
        }
        if past_header {
            let name = line.trim();
            if !name.is_empty() {
                accels.push(name.to_string());
            }
        }
    }

    accels
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

    fn description(&self) -> &str {
        env!("CARGO_PKG_DESCRIPTION")
    }

    fn author(&self) -> &str {
        env!("CARGO_PKG_AUTHORS")
    }

    fn license(&self) -> &str {
        env!("CARGO_PKG_LICENSE")
    }

    fn homepage(&self) -> &str {
        env!("CARGO_PKG_REPOSITORY")
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

    fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
        let codecs = Command::new("ffmpeg")
            .args(["-codecs", "-hide_banner"])
            .output()
            .map(|o| parse_codecs(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to probe ffmpeg codecs");
                CodecCapabilities::empty()
            });

        let formats = Command::new("ffmpeg")
            .args(["-formats", "-hide_banner"])
            .output()
            .map(|o| parse_formats(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to probe ffmpeg formats");
                Vec::new()
            });

        let hw_accels = Command::new("ffmpeg")
            .args(["-hwaccels", "-hide_banner"])
            .output()
            .map(|o| parse_hwaccels(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to probe ffmpeg hwaccels");
                Vec::new()
            });

        self.probed_codecs = Some(codecs.clone());
        self.probed_formats = Some(formats.clone());
        self.probed_hw_accels = Some(hw_accels.clone());

        // Detect HW accel backend for use during execution
        self.hw_accel = HwAccelConfig::detect();

        let event = ExecutorCapabilitiesEvent::new("ffmpeg-executor", codecs, formats, hw_accels);

        Ok(vec![Event::ExecutorCapabilities(event)])
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
        let mut plan = Plan::new(file, "test", "process");
        plan.actions = actions;
        plan
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
            other => panic!("Expected Execute capability, got {other:?}"),
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
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode to HEVC",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_transcode_audio() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeAudio,
                1,
                ActionParams::Transcode {
                    codec: "opus".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode to Opus",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_synthesize_audio() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::SynthesizeAudio,
                1,
                ActionParams::Synthesize {
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
                "Synthesize audio",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_convert_container() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::file_op(
                OperationType::ConvertContainer,
                ActionParams::Container {
                    container: Container::Mkv,
                },
                "Convert to MKV",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_non_mkv_metadata() {
        let plugin = FfmpegExecutorPlugin::new();
        // MP4 file with metadata ops — FFmpeg handles non-MKV metadata
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::SetDefault,
                1,
                ActionParams::Empty,
                "Set default",
            )],
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
                PlannedAction::track_op(
                    OperationType::TranscodeVideo,
                    0,
                    ActionParams::Transcode {
                        codec: "h264".into(),
                        crf: None,
                        preset: None,
                        bitrate: None,
                        channels: None,
                    },
                    "Transcode to H.264",
                ),
                PlannedAction::track_op(
                    OperationType::SetDefault,
                    1,
                    ActionParams::Empty,
                    "Set default",
                ),
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
                PlannedAction::track_op(
                    OperationType::SetDefault,
                    1,
                    ActionParams::Empty,
                    "Set default",
                ),
                PlannedAction::track_op(
                    OperationType::SetTitle,
                    1,
                    ActionParams::Title {
                        title: "English".into(),
                    },
                    "Set title",
                ),
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
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode",
            )],
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
            vec![PlannedAction::track_op(
                OperationType::SetDefault,
                1,
                ActionParams::Empty,
                "Set default",
            )],
        );
        assert!(plugin.execute_plan(&plan).is_err());
    }

    #[test]
    fn test_execute_plan_file_not_found() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(
            sample_mp4_file(), // /media/video.mp4 does not exist
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: Some(23),
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode to HEVC",
            )],
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
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode to HEVC",
            )],
        );

        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
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
            vec![PlannedAction::track_op(
                OperationType::SetDefault,
                1,
                ActionParams::Empty,
                "Set default",
            )],
        );

        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_ignores_other_events() {
        let plugin = FfmpegExecutorPlugin::new();
        let event = Event::PlanExecuting(PlanExecutingEvent::new(
            PathBuf::from("/test.mp4"),
            "process",
            1,
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_empty_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let plan = plan_with_actions(sample_mp4_file(), vec![]);
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_skipped_plan() {
        let plugin = FfmpegExecutorPlugin::new();
        let mut plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                },
                "Transcode",
            )],
        );
        plan.skip_reason = Some("Already processed".into());
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_default_impl() {
        let plugin = FfmpegExecutorPlugin::default();
        assert_eq!(plugin.name(), "ffmpeg-executor");
    }

    // ── codec/format/hwaccel parsing ────────────────────────────

    #[test]
    fn test_parse_codecs() {
        let output = "\
Codecs:
 -------
 DEVIL. h264                 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 DEV.L. hevc                 H.265 / HEVC
 D.A.L. aac                  AAC (Advanced Audio Coding)
 .EA.L. opus                 Opus (Opus Interactive Audio Codec)
 ..S... srt                  SubRip subtitle
";
        let caps = parse_codecs(output);
        assert!(caps.decoders.contains(&"h264".to_string()));
        assert!(caps.decoders.contains(&"hevc".to_string()));
        assert!(caps.decoders.contains(&"aac".to_string()));
        assert!(!caps.decoders.contains(&"opus".to_string()));
        assert!(caps.encoders.contains(&"h264".to_string()));
        assert!(caps.encoders.contains(&"hevc".to_string()));
        assert!(caps.encoders.contains(&"opus".to_string()));
        assert!(!caps.encoders.contains(&"aac".to_string()));
    }

    #[test]
    fn test_parse_codecs_empty_output() {
        let caps = parse_codecs("");
        assert!(caps.decoders.is_empty());
        assert!(caps.encoders.is_empty());
    }

    #[test]
    fn test_parse_formats() {
        let output = "\
File formats:
 -------
 DE matroska,webm  Matroska / WebM
  E mp4            MP4 (MPEG-4 Part 14)
 D  avi            AVI (Audio Video Interleaved)
 DE flac           raw FLAC
";
        let formats = parse_formats(output);
        assert!(formats.contains(&"matroska".to_string()));
        assert!(formats.contains(&"webm".to_string()));
        assert!(formats.contains(&"mp4".to_string()));
        assert!(formats.contains(&"avi".to_string()));
        assert!(formats.contains(&"flac".to_string()));
    }

    #[test]
    fn test_parse_formats_empty_output() {
        let formats = parse_formats("");
        assert!(formats.is_empty());
    }

    #[test]
    fn test_parse_hwaccels() {
        let output = "\
Hardware acceleration methods:
videotoolbox
cuda
vaapi
";
        let accels = parse_hwaccels(output);
        assert_eq!(accels, vec!["videotoolbox", "cuda", "vaapi"]);
    }

    #[test]
    fn test_parse_hwaccels_empty_output() {
        let accels = parse_hwaccels("");
        assert!(accels.is_empty());
    }

    #[test]
    fn test_parse_hwaccels_no_methods() {
        let output = "Hardware acceleration methods:\n";
        let accels = parse_hwaccels(output);
        assert!(accels.is_empty());
    }
}
