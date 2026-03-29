//! `FFmpeg` Executor Plugin.
//!
//! Executes media plans using `FFmpeg` for transcoding, container conversion,
//! and metadata operations on non-MKV files (or any file requiring transcode).

pub mod command;
pub mod executor;
pub mod hwaccel;
pub mod probe;
pub mod progress;

use std::process::Command;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{
    CodecCapabilities, Event, EventResult, ExecutorCapabilitiesEvent, PlanCreatedEvent,
};
use voom_domain::media::Container;
use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};
use voom_kernel::{Plugin, PluginContext};

use crate::hwaccel::HwAccelConfig;
use crate::probe::{parse_codecs, parse_formats, parse_hw_implementations, parse_hwaccels};

pub(crate) fn plugin_err(message: impl Into<String>) -> VoomError {
    VoomError::plugin("ffmpeg-executor", message)
}

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
    /// - Plans requiring codecs/formats the probed FFmpeg doesn't support
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
            // When probed data exists, verify the required codecs/formats
            if !self.can_handle_probed(plan) {
                return false;
            }
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

    /// Check probed codec/format data against the plan's requirements.
    ///
    /// Returns `true` if probing wasn't performed (graceful fallback)
    /// or if all required codecs and formats are supported.
    fn can_handle_probed(&self, plan: &Plan) -> bool {
        for action in &plan.actions {
            match (&action.operation, &action.parameters) {
                (
                    OperationType::TranscodeVideo | OperationType::TranscodeAudio,
                    ActionParams::Transcode { codec, .. },
                ) => {
                    if let Some(caps) = &self.probed_codecs {
                        if !caps.encoders.iter().any(|e| e == codec) {
                            tracing::debug!(
                                codec = %codec,
                                "rejecting plan: codec not in probed encoders"
                            );
                            return false;
                        }
                    }
                    // Verify the source codec has a decoder
                    if !self.has_decoder_for_track(plan, action) {
                        return false;
                    }
                }
                (
                    OperationType::SynthesizeAudio,
                    ActionParams::Synthesize {
                        codec: Some(codec), ..
                    },
                ) => {
                    if let Some(caps) = &self.probed_codecs {
                        if !caps.encoders.iter().any(|e| e == codec) {
                            tracing::debug!(
                                codec = %codec,
                                "rejecting plan: synthesize codec not in probed encoders"
                            );
                            return false;
                        }
                    }
                }
                (OperationType::ConvertContainer, ActionParams::Container { container }) => {
                    if let Some(formats) = &self.probed_formats {
                        if let Some(name) = container.ffmpeg_format_name() {
                            if !formats.iter().any(|f| f == name) {
                                tracing::debug!(
                                    format = %name,
                                    "rejecting plan: format not in probed formats"
                                );
                                return false;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        true
    }

    /// Check that ffmpeg has a decoder for the source track being
    /// transcoded.  Without one, ffmpeg fails with "no decoder found".
    fn has_decoder_for_track(&self, plan: &Plan, action: &PlannedAction) -> bool {
        let caps = match &self.probed_codecs {
            Some(c) => c,
            None => return true, // no probed data — optimistic
        };
        let idx = match action.track_index {
            Some(i) => i as usize,
            None => return true, // global codec override — skip check
        };
        let track = match plan.file.tracks.get(idx) {
            Some(t) => t,
            None => return true, // index out of range — let ffmpeg report
        };
        let source_codec = &track.codec;
        if caps.decoders.iter().any(|d| d == source_codec) {
            return true;
        }
        tracing::warn!(
            source_codec = %source_codec,
            track_index = idx,
            path = %plan.file.path.display(),
            "rejecting plan: no decoder for source codec \
             (ffmpeg may be missing non-free codecs — \
             install from rpmfusion.org on Fedora)"
        );
        false
    }

    /// Execute a plan using the ffmpeg executor module.
    pub fn execute_plan(&self, plan: &Plan) -> Result<Vec<voom_domain::plan::ActionResult>> {
        if !self.can_handle(plan) {
            return Err(plugin_err("Plan cannot be handled by FFmpeg executor"));
        }
        executor::execute_plan(plan, &self.hw_accel)
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

    voom_kernel::plugin_cargo_metadata!();

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
        let mut codecs = Command::new("ffmpeg")
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

        // Probe HW encoder/decoder implementations
        codecs.hw_encoders = Command::new("ffmpeg")
            .args(["-encoders", "-hide_banner"])
            .output()
            .map(|o| parse_hw_implementations(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to probe ffmpeg hw encoders");
                Vec::new()
            });

        codecs.hw_decoders = Command::new("ffmpeg")
            .args(["-decoders", "-hide_banner"])
            .output()
            .map(|o| parse_hw_implementations(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to probe ffmpeg hw decoders");
                Vec::new()
            });

        self.probed_codecs = Some(codecs.clone());
        self.probed_formats = Some(formats.clone());
        self.probed_hw_accels = Some(hw_accels.clone());

        // Select HW accel backend from already-probed data (no extra subprocess)
        self.hw_accel = HwAccelConfig::from_probed(&hw_accels);

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
    use voom_domain::plan::{ActionParams, PlannedAction};

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
                    hw: None,
                    hw_fallback: None,
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
                    hw: None,
                    hw_fallback: None,
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
                        hw: None,
                        hw_fallback: None,
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
                    hw: None,
                    hw_fallback: None,
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
                    hw: None,
                    hw_fallback: None,
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
                    hw: None,
                    hw_fallback: None,
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
                    hw: None,
                    hw_fallback: None,
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

    // ── can_handle: probed capability checks ───────────────────

    fn plugin_with_probed(encoders: Vec<&str>, formats: Vec<&str>) -> FfmpegExecutorPlugin {
        let enc: Vec<String> = encoders.into_iter().map(String::from).collect();
        let mut plugin = FfmpegExecutorPlugin::new();
        // Use encoders as decoders too — real ffmpeg typically has both
        plugin.probed_codecs = Some(CodecCapabilities::new(enc.clone(), enc));
        plugin.probed_formats = Some(formats.into_iter().map(String::from).collect());
        plugin
    }

    #[test]
    fn test_can_handle_rejects_unsupported_codec() {
        let plugin = plugin_with_probed(vec!["h264", "aac"], vec!["mp4"]);
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
                    hw: None,
                    hw_fallback: None,
                },
                "Transcode to HEVC",
            )],
        );
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_accepts_supported_codec() {
        let plugin = plugin_with_probed(vec!["h264", "hevc", "aac"], vec!["mp4"]);
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
                    hw: None,
                    hw_fallback: None,
                },
                "Transcode to HEVC",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_rejects_unsupported_format() {
        let plugin = plugin_with_probed(vec!["h264"], vec!["mp4", "matroska"]);
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::file_op(
                OperationType::ConvertContainer,
                ActionParams::Container {
                    container: Container::Webm,
                },
                "Convert to WebM",
            )],
        );
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_fallback_when_not_probed() {
        let plugin = FfmpegExecutorPlugin::new(); // no probed data
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "av1".into(),
                    crf: None,
                    preset: None,
                    bitrate: None,
                    channels: None,
                    hw: None,
                    hw_fallback: None,
                },
                "Transcode to AV1",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_synthesize_checks_codec() {
        let plugin = plugin_with_probed(vec!["h264", "aac"], vec!["mp4"]);
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::SynthesizeAudio,
                1,
                ActionParams::Synthesize {
                    name: "stereo".into(),
                    codec: Some("opus".into()),
                    language: None,
                    text: None,
                    bitrate: None,
                    channels: None,
                    title: None,
                    position: None,
                    source_track: None,
                },
                "Synthesize audio (opus)",
            )],
        );
        assert!(!plugin.can_handle(&plan));
    }
}
