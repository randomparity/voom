//! `FFmpeg` Executor Plugin.
//!
//! Executes media plans using `FFmpeg` for transcoding, container conversion,
//! and metadata operations on non-MKV files (or any file requiring transcode).

pub mod command;
pub mod executor;
pub mod hwaccel;
pub mod probe;
pub mod progress;
pub mod vmaf;

use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{
    CodecCapabilities, Event, EventResult, ExecutorCapabilitiesEvent, ExecutorParallelLimit,
    PlanCreatedEvent, PlanExecutingEvent,
};
use voom_domain::media::Container;
use voom_domain::plan::{ActionParams, OperationType, Plan, PlannedAction};
use voom_domain::temp_file::temp_path;
use voom_domain::utils::language::is_valid_language;
use voom_domain::utils::sanitize::validate_metadata_value;
use voom_kernel::{Plugin, PluginContext};

use crate::hwaccel::{resolve_hw_config, HwAccelBackend, HwAccelConfig};
use crate::probe::{
    enumerate_gpus, probe_capabilities, validate_hw_encoder, validate_hw_encoder_on_device,
    validate_hw_encoders_parallel, GpuDevice,
};

/// Per-plugin configuration from `[plugin.ffmpeg-executor]` in config.toml.
#[derive(Debug, Default, serde::Deserialize)]
struct FfmpegExecutorConfig {
    #[serde(default)]
    hw_accel: Option<String>,
    #[serde(default)]
    gpu_device: Option<String>,
    #[serde(default)]
    nvenc_max_parallel: Option<usize>,
    #[serde(default)]
    hw_decode: Option<bool>,
}

const DEFAULT_NVENC_MAX_PARALLEL_PER_GPU: usize = 4;

fn positive_or_default(value: Option<usize>, default: usize) -> usize {
    match value {
        Some(0) | None => default,
        Some(value) => value,
    }
}

fn nvenc_parallel_limits(
    backend: Option<HwAccelBackend>,
    validated_encoders: &[String],
    nvenc_max_parallel: Option<usize>,
) -> Vec<ExecutorParallelLimit> {
    if backend != Some(HwAccelBackend::Nvenc)
        || !validated_encoders
            .iter()
            .any(|encoder| encoder.ends_with("_nvenc"))
    {
        return Vec::new();
    }

    vec![ExecutorParallelLimit::new(
        "hw:nvenc",
        positive_or_default(nvenc_max_parallel, DEFAULT_NVENC_MAX_PARALLEL_PER_GPU),
    )]
}

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
    available: bool,
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
            available: false,
            hw_accel: HwAccelConfig::new(),
            probed_codecs: None,
            probed_formats: None,
            probed_hw_accels: None,
        }
    }

    /// Create with `available` set to the given value.
    /// Bypasses the `init()` probe for testing.
    #[cfg(test)]
    fn with_available(mut self, available: bool) -> Self {
        self.available = available;
        self
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
    /// - Plans requiring codecs/formats the probed `FFmpeg` doesn't support
    #[must_use]
    pub fn can_handle(&self, plan: &Plan) -> bool {
        if !self.available || plan.is_empty() || plan.is_skipped() {
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

        // MuxSubtitle on non-MKV files
        let has_mux_subtitle = plan
            .actions
            .iter()
            .any(|a| a.operation == OperationType::MuxSubtitle);
        if has_mux_subtitle {
            let is_mkv = plan.file.container == Container::Mkv;
            return !is_mkv;
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

        // Handle MuxSubtitle actions directly (not via the general ffmpeg command builder)
        if plan
            .actions
            .iter()
            .any(|a| a.operation == OperationType::MuxSubtitle)
        {
            let mut results = Vec::new();
            for action in plan
                .actions
                .iter()
                .filter(|a| a.operation == OperationType::MuxSubtitle)
            {
                results.extend(self.execute_mux_subtitle(&plan.file.path, action)?);
            }
            return Ok(results);
        }

        executor::execute_plan(plan, &self.hw_accel)
    }

    /// Execute a `MuxSubtitle` action by running ffmpeg.
    fn execute_mux_subtitle(
        &self,
        path: &std::path::Path,
        action: &PlannedAction,
    ) -> Result<Vec<voom_domain::plan::ActionResult>> {
        let ActionParams::MuxSubtitle {
            subtitle_path,
            language,
            forced,
            title: _,
        } = &action.parameters
        else {
            return Err(plugin_err("expected MuxSubtitle params"));
        };

        if !is_valid_language(language) {
            return Err(plugin_err(format!(
                "invalid ISO 639 language code: \"{language}\""
            )));
        }

        let temp_path = temp_path(path);
        let mut args = vec![
            "-i".to_string(),
            path.to_string_lossy().into_owned(),
            "-i".to_string(),
            subtitle_path.to_string_lossy().into_owned(),
            "-c".to_string(),
            "copy".to_string(),
            "-c:s".to_string(),
            "srt".to_string(),
            "-metadata:s:s:0".to_string(),
            format!("language={language}"),
        ];
        if *forced {
            args.push("-disposition:s:0".to_string());
            args.push("forced".to_string());
        }
        args.push("-y".to_string());
        args.push(temp_path.to_string_lossy().into_owned());

        let command_str = voom_process::shell_quote_args("ffmpeg", &args);
        const SUBTITLE_MUX_TIMEOUT: Duration = Duration::from_secs(120);
        let start = std::time::Instant::now();
        let output = voom_process::run_with_timeout_env("ffmpeg", &args, SUBTITLE_MUX_TIMEOUT, &[]);
        let duration_ms = start.elapsed().as_millis() as u64;

        match output {
            Ok(o) if o.status.success() => {
                std::fs::rename(&temp_path, path).map_err(|e| {
                    let _ = std::fs::remove_file(&temp_path);
                    plugin_err(format!("failed to rename temp file: {e}"))
                })?;
                let detail = voom_domain::plan::ExecutionDetail {
                    command: command_str,
                    exit_code: Some(0),
                    stderr_tail: String::new(),
                    duration_ms,
                };
                Ok(vec![voom_domain::plan::ActionResult::success(
                    action.operation,
                    &action.description,
                )
                .with_execution_detail(detail)])
            }
            Ok(o) => {
                let _ = std::fs::remove_file(&temp_path);
                let tail = voom_process::stderr_tail(&o.stderr, 20);
                let display_tail = if tail.is_empty() {
                    "(no output)"
                } else {
                    &tail
                };
                let error_msg = format!(
                    "ffmpeg failed (exit {}):\n{}\ncmd: {}",
                    o.status.code().unwrap_or(-1),
                    display_tail,
                    command_str
                );
                let detail = voom_domain::plan::ExecutionDetail {
                    command: command_str,
                    exit_code: o.status.code(),
                    stderr_tail: tail,
                    duration_ms,
                };
                Ok(vec![voom_domain::plan::ActionResult::failure(
                    action.operation,
                    &action.description,
                    &error_msg,
                )
                .with_execution_detail(detail)])
            }
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                Err(plugin_err(format!("ffmpeg subtitle mux failed: {e}")))
            }
        }
    }

    /// Handle a `subtitle.generated` event for non-MKV files.
    ///
    /// Converts the event into a `PlanCreated` event with a `MuxSubtitle`
    /// action. Emits `PlanExecuting` before `PlanCreated` so that
    /// backup-manager creates a backup before the executor modifies the file.
    fn handle_subtitle_generated(
        &self,
        event: &voom_domain::events::SubtitleGeneratedEvent,
    ) -> Result<Option<EventResult>> {
        if !self.available {
            return Ok(None);
        }

        // Defer MKV files to mkvtoolnix
        let ext = event
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let container = Container::from_extension(ext);
        if container == Container::Mkv {
            return Ok(None);
        }

        validate_metadata_value(&event.language)
            .map_err(|e| plugin_err(format!("invalid language: {e}")))?;
        if let Some(title) = &event.title {
            validate_metadata_value(title)
                .map_err(|e| plugin_err(format!("invalid title: {e}")))?;
        }

        let phase_name = "subtitle_mux";
        let mut file = voom_domain::media::MediaFile::new(event.path.clone());
        file.container = container;
        let mut plan = Plan::new(file, "subtitle-mux", phase_name);
        plan.actions = vec![PlannedAction::file_op(
            OperationType::MuxSubtitle,
            ActionParams::MuxSubtitle {
                subtitle_path: event.subtitle_path.clone(),
                language: event.language.clone(),
                forced: event.forced,
                title: event.title.clone(),
            },
            "Mux subtitle into container",
        )];

        let produced_events = vec![
            Event::PlanExecuting(PlanExecutingEvent::new(
                plan.id,
                event.path.clone(),
                phase_name,
                1,
            )),
            Event::PlanCreated(voom_domain::events::PlanCreatedEvent::new(plan)),
        ];

        let mut result = EventResult::new("ffmpeg-executor");
        result.claimed = true;
        result.produced_events = produced_events;
        Ok(Some(result))
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
    fn name(&self) -> &'static str {
        "ffmpeg-executor"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        if self.available {
            &self.capabilities
        } else {
            &[]
        }
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED || event_type == Event::SUBTITLE_GENERATED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanCreated(plan_event) => self.handle_plan_created(plan_event),
            Event::SubtitleGenerated(e) => self.handle_subtitle_generated(e),
            _ => Ok(None),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        let caps = probe_capabilities();

        let Some(codecs) = caps.codecs else {
            tracing::warn!("ffmpeg not found; ffmpeg executor disabled");
            return Ok(vec![]);
        };
        self.available = true;

        let formats = caps.formats;
        let hw_accels = caps.hw_accels;

        self.probed_codecs = Some(codecs.clone());
        self.probed_formats = Some(formats.clone());
        self.probed_hw_accels = Some(hw_accels.clone());

        // Read per-plugin config for GPU device selection and hw_accel override
        let plugin_config = match ctx.parse_config::<FfmpegExecutorConfig>() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("ffmpeg-executor config parse failed, using defaults: {e}");
                FfmpegExecutorConfig::default()
            }
        };

        // Select HW accel backend: config override or auto-detect
        let (mut hw_config, _source) =
            resolve_hw_config(plugin_config.hw_accel.as_deref(), &hw_accels);

        // Resolve configured GPU device
        let target_device: Option<GpuDevice> = if let (Some(backend), Some(device_id)) =
            (hw_config.backend, &plugin_config.gpu_device)
        {
            let gpus = enumerate_gpus(backend);
            let found = gpus.into_iter().find(|g| g.id == *device_id);
            if found.is_some() {
                tracing::info!(
                    device = %device_id,
                    "using configured GPU device"
                );
                hw_config = hw_config.with_device(Some(device_id.clone()));
            } else {
                tracing::warn!(
                    device = %device_id,
                    "configured gpu_device not found, using system default"
                );
            }
            found
        } else {
            None
        };

        // Validate which HW encoders actually work on the target device.
        // ffmpeg -encoders lists all compiled-in encoders, but the GPU may
        // not support all of them (e.g. av1_nvenc on a GPU without AV1).
        let validated_encoders: Vec<String> = match (&hw_config.backend, &target_device) {
            (Some(backend), Some(device)) => {
                validate_hw_encoders_parallel(&codecs.hw_encoders, |enc| {
                    validate_hw_encoder_on_device(enc, *backend, device)
                })
            }
            _ => validate_hw_encoders_parallel(&codecs.hw_encoders, validate_hw_encoder),
        };

        tracing::info!(
            validated = ?validated_encoders,
            total = codecs.hw_encoders.len(),
            "validated HW encoders"
        );

        let parallel_limits = nvenc_parallel_limits(
            hw_config.backend,
            &validated_encoders,
            plugin_config.nvenc_max_parallel,
        );

        self.hw_accel = hw_config
            .with_validated_encoders(validated_encoders)
            .with_hw_decoders(codecs.hw_decoders.clone())
            .with_hw_decode_enabled(plugin_config.hw_decode.unwrap_or(true));

        let event = ExecutorCapabilitiesEvent::new("ffmpeg-executor", codecs, formats, hw_accels)
            .with_parallel_limits(parallel_limits);

        Ok(vec![Event::ExecutorCapabilities(event)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::PlanExecutingEvent;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::{ActionParams, PlannedAction, TranscodeSettings};

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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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

    #[test]
    fn test_handles_subtitle_generated() {
        let plugin = FfmpegExecutorPlugin::new();
        assert!(plugin.handles(Event::SUBTITLE_GENERATED));
    }

    #[test]
    fn test_subtitle_generated_mkv_returns_none() {
        let plugin = FfmpegExecutorPlugin::new();
        let event = Event::SubtitleGenerated(voom_domain::events::SubtitleGeneratedEvent::new(
            PathBuf::from("/media/movie.mkv"),
            PathBuf::from("/media/movie.forced-eng.srt"),
            "eng",
            true,
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    // ── can_handle: positive cases ──────────────────────────────

    #[test]
    fn test_can_handle_transcode_video() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: Default::default(),
                },
                "Transcode to HEVC",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_transcode_audio() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeAudio,
                1,
                ActionParams::Transcode {
                    codec: "opus".into(),
                    settings: Default::default(),
                },
                "Transcode to Opus",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_can_handle_synthesize_audio() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        // MKV file with transcode — FFmpeg handles all transcodes regardless of container
        let plan = plan_with_actions(
            sample_mkv_file(),
            vec![
                PlannedAction::track_op(
                    OperationType::TranscodeVideo,
                    0,
                    ActionParams::Transcode {
                        codec: "h264".into(),
                        settings: Default::default(),
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(sample_mp4_file(), vec![]);
        assert!(!plugin.can_handle(&plan));
    }

    #[test]
    fn test_cannot_handle_skipped_plan() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let mut plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: Default::default(),
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(
            sample_mp4_file(), // /media/video.mp4 does not exist
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: TranscodeSettings::default().with_crf(Some(23)),
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: Default::default(),
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
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
            uuid::Uuid::new_v4(),
            PathBuf::from("/test.mp4"),
            "process",
            1,
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_empty_plan() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let plan = plan_with_actions(sample_mp4_file(), vec![]);
        let event = Event::PlanCreated(PlanCreatedEvent::new(plan));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_skips_skipped_plan() {
        let plugin = FfmpegExecutorPlugin::new().with_available(true);
        let mut plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: Default::default(),
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
        let mut plugin = FfmpegExecutorPlugin::new().with_available(true);
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
                    settings: Default::default(),
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
                    settings: Default::default(),
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
        let plugin = FfmpegExecutorPlugin::new().with_available(true); // no probed data
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "av1".into(),
                    settings: Default::default(),
                },
                "Transcode to AV1",
            )],
        );
        assert!(plugin.can_handle(&plan));
    }

    #[test]
    fn test_ffmpeg_executor_config_deserialize() {
        let json = serde_json::json!({"gpu_device": "1"});
        let config: FfmpegExecutorConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.gpu_device.as_deref(), Some("1"));
        assert!(config.hw_accel.is_none());
    }

    #[test]
    fn test_ffmpeg_executor_config_nvenc_max_parallel() {
        let json = serde_json::json!({
            "hw_accel": "nvenc",
            "gpu_device": "0",
            "nvenc_max_parallel": 3
        });
        let config: FfmpegExecutorConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.hw_accel.as_deref(), Some("nvenc"));
        assert_eq!(config.gpu_device.as_deref(), Some("0"));
        assert_eq!(config.nvenc_max_parallel, Some(3));
    }

    #[test]
    fn test_ffmpeg_executor_config_hw_accel() {
        let json = serde_json::json!({"hw_accel": "vaapi", "gpu_device": "/dev/dri/renderD128"});
        let config: FfmpegExecutorConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.hw_accel.as_deref(), Some("vaapi"));
        assert_eq!(config.gpu_device.as_deref(), Some("/dev/dri/renderD128"));
    }

    #[test]
    fn test_ffmpeg_executor_config_hw_decode() {
        let json = serde_json::json!({"hw_decode": false});
        let config: FfmpegExecutorConfig = serde_json::from_value(json).unwrap();

        assert_eq!(config.hw_decode, Some(false));
    }

    #[test]
    fn test_ffmpeg_executor_config_default() {
        let config = FfmpegExecutorConfig::default();
        assert!(config.gpu_device.is_none());
        assert!(config.hw_accel.is_none());
    }

    #[test]
    fn test_config_nvenc_parallel_limits_use_configured_capacity() {
        let limits = nvenc_parallel_limits(
            Some(crate::hwaccel::HwAccelBackend::Nvenc),
            &["h264_nvenc".to_string()],
            Some(3),
        );

        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].resource, "hw:nvenc");
        assert_eq!(limits[0].max_parallel, 3);
    }

    #[test]
    fn test_config_nvenc_parallel_limits_default_missing_capacity() {
        let limits = nvenc_parallel_limits(
            Some(crate::hwaccel::HwAccelBackend::Nvenc),
            &["h264_nvenc".to_string()],
            None,
        );

        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].max_parallel, DEFAULT_NVENC_MAX_PARALLEL_PER_GPU);
    }

    #[test]
    fn test_config_nvenc_parallel_limits_default_zero_capacity() {
        let limits = nvenc_parallel_limits(
            Some(crate::hwaccel::HwAccelBackend::Nvenc),
            &["h264_nvenc".to_string()],
            Some(0),
        );

        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].max_parallel, DEFAULT_NVENC_MAX_PARALLEL_PER_GPU);
    }

    #[test]
    fn test_config_nvenc_parallel_limits_skip_empty_encoders() {
        let limits =
            nvenc_parallel_limits(Some(crate::hwaccel::HwAccelBackend::Nvenc), &[], Some(3));

        assert!(limits.is_empty());
    }

    #[test]
    fn test_config_nvenc_parallel_limits_skip_non_nvenc_encoder() {
        let limits = nvenc_parallel_limits(
            Some(crate::hwaccel::HwAccelBackend::Nvenc),
            &["h264_vaapi".to_string()],
            Some(3),
        );

        assert!(limits.is_empty());
    }

    #[test]
    fn test_config_nvenc_parallel_limits_skip_non_nvenc_backend() {
        let limits = nvenc_parallel_limits(
            Some(crate::hwaccel::HwAccelBackend::Vaapi),
            &["h264_nvenc".to_string()],
            Some(3),
        );

        assert!(limits.is_empty());
    }

    #[test]
    fn test_can_handle_unavailable() {
        let plugin = FfmpegExecutorPlugin::new(); // available = false
        let plan = plan_with_actions(
            sample_mp4_file(),
            vec![PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: Default::default(),
                },
                "Transcode to HEVC",
            )],
        );
        assert!(!plugin.can_handle(&plan));
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
