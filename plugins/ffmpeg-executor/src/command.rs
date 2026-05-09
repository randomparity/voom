//! `FFmpeg` command builder with safe argument construction.

use std::path::Path;

use voom_domain::errors::{Result, VoomError};
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::{
    ActionParams, LoudnessNormalization, OperationType, PlannedAction, TranscodeChannels,
};
use voom_domain::utils::sanitize::{validate_metadata_key, validate_metadata_value};

use crate::hwaccel::{self, HwAccelConfig};

/// A builder for constructing `FFmpeg` command-line arguments.
#[derive(Debug, Clone)]
pub struct FfmpegCommand {
    args: Vec<String>,
}

impl FfmpegCommand {
    /// Create a new command with default flags (`-y -hide_banner`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            args: vec!["-y".to_string(), "-hide_banner".to_string()],
        }
    }

    /// Add an input file.
    #[must_use]
    pub fn input(mut self, path: &Path) -> Self {
        self.args.push("-i".to_string());
        self.args.push(path.to_string_lossy().to_string());
        self
    }

    /// Set the output file (must be called last before `build`).
    #[must_use]
    pub fn output(mut self, path: &Path) -> Self {
        self.args.push(path.to_string_lossy().to_string());
        self
    }

    /// Map all streams from input (`-map 0`).
    #[must_use]
    pub fn map_all(mut self) -> Self {
        self.args.push("-map".to_string());
        self.args.push("0".to_string());
        self
    }

    /// Map a specific track by index (`-map 0:{index}`).
    #[must_use]
    pub fn map_track(mut self, index: u32) -> Self {
        self.args.push("-map".to_string());
        self.args.push(format!("0:{index}"));
        self
    }

    /// Copy all codecs (`-c copy`).
    #[must_use]
    pub fn codec_copy(mut self) -> Self {
        self.args.push("-c".to_string());
        self.args.push("copy".to_string());
        self
    }

    /// Set global video codec (`-c:v {codec}`).
    #[must_use]
    pub fn video_codec(mut self, codec: &str) -> Self {
        self.args.push("-c:v".to_string());
        self.args.push(codec.to_string());
        self
    }

    /// Set global audio codec (`-c:a {codec}`).
    #[must_use]
    pub fn audio_codec(mut self, codec: &str) -> Self {
        self.args.push("-c:a".to_string());
        self.args.push(codec.to_string());
        self
    }

    /// Set video codec for a specific stream (`-c:v:{stream} {codec}`).
    #[must_use]
    pub fn video_codec_for_track(mut self, stream: u32, codec: &str) -> Self {
        self.args.push(format!("-c:v:{stream}"));
        self.args.push(codec.to_string());
        self
    }

    /// Set audio codec for a specific stream (`-c:a:{stream} {codec}`).
    #[must_use]
    pub fn audio_codec_for_track(mut self, stream: u32, codec: &str) -> Self {
        self.args.push(format!("-c:a:{stream}"));
        self.args.push(codec.to_string());
        self
    }

    /// Set CRF value (`-crf {value}`).
    #[must_use]
    pub fn crf(mut self, value: u32) -> Self {
        self.args.push("-crf".to_string());
        self.args.push(value.to_string());
        self
    }

    /// Set audio bitrate (`-b:a {bitrate}`).
    #[must_use]
    pub fn audio_bitrate(mut self, bitrate: &str) -> Self {
        self.args.push("-b:a".to_string());
        self.args.push(bitrate.to_string());
        self
    }

    /// Set encoding preset (`-preset {preset}`).
    #[must_use]
    pub fn preset(mut self, preset: &str) -> Self {
        self.args.push("-preset".to_string());
        self.args.push(preset.to_string());
        self
    }

    /// Set metadata on a stream or globally.
    ///
    /// With `stream_index`: `-metadata:s:{index} {key}={value}`
    /// Without: `-metadata {key}={value}`
    #[must_use]
    pub fn metadata(mut self, stream_index: Option<u32>, key: &str, value: &str) -> Self {
        match stream_index {
            Some(idx) => {
                self.args.push(format!("-metadata:s:{idx}"));
            }
            None => {
                self.args.push("-metadata".to_string());
            }
        }
        self.args.push(format!("{key}={value}"));
        self
    }

    /// Set disposition on a stream (`-disposition:{stream_index} {value}`).
    #[must_use]
    pub fn disposition(mut self, stream_index: u32, value: &str) -> Self {
        self.args.push(format!("-disposition:{stream_index}"));
        self.args.push(value.to_string());
        self
    }

    /// Clear a metadata key (set to empty value: `-metadata key=`).
    #[must_use]
    pub fn clear_metadata(mut self, key: &str) -> Self {
        self.args.push("-metadata".to_string());
        self.args.push(format!("{key}="));
        self
    }

    /// Enable progress output to pipe (`-progress pipe:1`).
    #[must_use]
    pub fn progress_pipe(mut self) -> Self {
        self.args.push("-progress".to_string());
        self.args.push("pipe:1".to_string());
        self
    }

    /// Add a video filter (`-vf {filter}`).
    #[must_use]
    pub fn video_filter(mut self, filter: &str) -> Self {
        self.args.push("-vf".to_string());
        self.args.push(filter.to_string());
        self
    }

    /// Set output color metadata for the encoded video stream.
    #[must_use]
    pub fn video_color_metadata(mut self, primaries: &str, transfer: &str, matrix: &str) -> Self {
        self.args.push("-color_primaries".to_string());
        self.args.push(primaries.to_string());
        self.args.push("-color_trc".to_string());
        self.args.push(transfer.to_string());
        self.args.push("-colorspace".to_string());
        self.args.push(matrix.to_string());
        self
    }

    /// Add x265 encoder parameters.
    #[must_use]
    pub fn x265_params(mut self, params: &str) -> Self {
        self.args.push("-x265-params".to_string());
        self.args.push(params.to_string());
        self
    }

    /// Add an audio filter for a stream or for all audio streams.
    #[must_use]
    pub fn audio_filter(mut self, stream: Option<u32>, filter: &str) -> Self {
        let flag = stream.map_or_else(|| "-af".to_string(), |idx| format!("-filter:a:{idx}"));
        self.args.push(flag);
        self.args.push(filter.to_string());
        self
    }

    /// Add a raw argument.
    #[must_use]
    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

    /// Consume the builder and return the argument list.
    #[must_use]
    pub fn build(self) -> Vec<String> {
        self.args
    }
}

impl Default for FfmpegCommand {
    fn default() -> Self {
        Self::new()
    }
}

fn apply_audio_codec_args(
    mut cmd: FfmpegCommand,
    stream: Option<u32>,
    encoder: &str,
    bitrate: Option<&str>,
    channels: Option<u32>,
    loudness: Option<&LoudnessNormalization>,
) -> FfmpegCommand {
    if let Some(stream) = stream {
        cmd = cmd.audio_codec_for_track(stream, encoder);
    } else {
        cmd = cmd.audio_codec(encoder);
    }

    if let Some(brate) = bitrate {
        cmd = cmd.audio_bitrate(brate);
    }

    if let Some(ch) = channels {
        cmd = cmd.arg("-ac").arg(&ch.to_string());
    }

    if let Some(settings) = loudness {
        cmd = cmd.audio_filter(stream, &loudnorm_filter(settings));
    }

    cmd
}

fn loudnorm_filter(settings: &LoudnessNormalization) -> String {
    let lra = settings.lra_max.unwrap_or(99.0);
    let base = format!(
        "loudnorm=I={:.1}:TP={:.1}:LRA={:.1}",
        settings.target_lufs, settings.true_peak_db, lra
    );
    let Some(measured) = &settings.measured else {
        return base;
    };
    format!(
        "{base}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:\
         measured_thresh={:.2}:offset={:.2}:linear=true:print_format=summary",
        measured.input_i,
        measured.input_tp,
        measured.input_lra,
        measured.input_thresh,
        measured.target_offset
    )
}

/// Emit the correct quality parameter for the chosen encoder.
///
/// Software encoders (libx264, libx265, libsvtav1, libvpx-vp9) accept
/// `-crf`.  Hardware encoders use backend-specific flags:
///   - NVENC: `-cq <val>` (VBR constant-quality mode)
///   - QSV:  `-global_quality <val>`
///   - VAAPI: `-rc_mode CQP -qp <val>`
///   - `VideoToolbox`: `-q:v <val>`
fn apply_quality(encoder: &str, mut cmd: FfmpegCommand, crf: u32) -> FfmpegCommand {
    if encoder.ends_with("_nvenc") {
        cmd = cmd.arg("-cq").arg(&crf.to_string());
    } else if encoder.ends_with("_qsv") {
        cmd = cmd.arg("-global_quality").arg(&crf.to_string());
    } else if encoder.ends_with("_vaapi") {
        cmd = cmd
            .arg("-rc_mode")
            .arg("CQP")
            .arg("-qp")
            .arg(&crf.to_string());
    } else if encoder.ends_with("_videotoolbox") {
        cmd = cmd.arg("-q:v").arg(&crf.to_string());
    } else {
        cmd = cmd.crf(crf);
    }
    cmd
}

/// Emit the correct preset flag for the chosen encoder.
///
/// VAAPI encoders do not support presets at all (flag is silently dropped).
/// All other encoders accept `-preset` directly — NVENC maps legacy names
/// (slow, medium, fast) to its own p1–p7 presets internally.
fn apply_preset(encoder: &str, mut cmd: FfmpegCommand, preset: &str) -> FfmpegCommand {
    if !encoder.ends_with("_vaapi") {
        cmd = cmd.preset(preset);
    }
    cmd
}

/// Emit `-tune` for software encoders only.
///
/// Hardware encoders (NVENC, QSV, VAAPI, `VideoToolbox`) do not support
/// the `-tune` flag, so it is silently skipped for those backends.
fn apply_tune(encoder: &str, mut cmd: FfmpegCommand, tune: &str) -> FfmpegCommand {
    if !encoder.ends_with("_nvenc")
        && !encoder.ends_with("_qsv")
        && !encoder.ends_with("_vaapi")
        && !encoder.ends_with("_videotoolbox")
    {
        cmd = cmd.arg("-tune").arg(tune);
    }
    cmd
}

fn encoder_supports_hdr(codec: &str) -> bool {
    matches!(codec, "hevc" | "h265" | "av1" | "h264")
}

/// Parse a max-resolution spec into a pixel height.
fn parse_max_height(spec: &str) -> Option<u32> {
    match spec.to_lowercase().as_str() {
        "4k" => Some(2160),
        "8k" => Some(4320),
        s => s.strip_suffix('p')?.parse().ok(),
    }
}

/// Resolve the effective HW acceleration config for a transcode action.
///
/// Per-action `hw` overrides take precedence over the system-wide config.
/// Returns `None` when the action explicitly requests `hw: "none"`.
fn resolve_effective_hw<'a>(
    settings: &voom_domain::plan::TranscodeSettings,
    hw_accel: Option<&'a HwAccelConfig>,
    owned_config: &'a mut Option<HwAccelConfig>,
) -> Option<&'a HwAccelConfig> {
    match settings.hw.as_deref() {
        Some("none") => None,
        Some(backend) => {
            let cfg = hwaccel::config_from_backend_with_system(backend, hw_accel);
            if cfg.enabled() {
                *owned_config = Some(cfg);
                owned_config.as_ref()
            } else {
                hw_accel
            }
        }
        None => hw_accel,
    }
}

/// Build the list of video filters from transcode settings.
///
/// Combines crop, scale (from `max_resolution`), and tonemap (from `hdr_mode`)
/// filters into a single list suitable for joining with commas.
fn collect_video_filters(file: &MediaFile, action: &PlannedAction) -> Vec<String> {
    let ActionParams::Transcode { settings, .. } = &action.parameters else {
        return Vec::new();
    };
    let mut filters: Vec<String> = Vec::new();

    if let Some(filter) = crop_filter(file, action, settings) {
        filters.push(filter);
    }

    if let Some(ref max_res) = settings.max_resolution {
        if let Some(max_h) = parse_max_height(max_res) {
            let algo = settings.scale_algorithm.as_deref().unwrap_or("lanczos");
            filters.push(format!("scale=-2:'min(ih,{max_h})':flags={algo}"));
        }
    }

    let explicit_tonemap = settings.hdr_mode.as_deref() == Some("tonemap");
    let source_is_hdr = source_video_track(file, action).is_some_and(|t| t.is_hdr);
    if should_tonemap(settings) && (explicit_tonemap || source_is_hdr) {
        let algorithm = settings.tonemap.as_deref().unwrap_or("bt2390");
        filters.push(tone_map_filter(algorithm));
    }

    filters
}

fn should_tonemap(settings: &voom_domain::plan::TranscodeSettings) -> bool {
    settings.hdr_mode.as_deref() == Some("tonemap")
        || settings.preserve_hdr == Some(false)
        || settings.tonemap.is_some()
}

fn should_preserve_hdr(
    source_track: &Track,
    settings: &voom_domain::plan::TranscodeSettings,
) -> bool {
    source_track.is_hdr && !should_tonemap(settings) && settings.preserve_hdr.unwrap_or(true)
}

fn tone_map_filter(algorithm: &str) -> String {
    let algorithm = if algorithm == "bt2390" {
        "bt2390"
    } else {
        algorithm
    };
    format!(
        "zscale=t=linear:npl=100,format=gbrpf32le,\
         zscale=p=bt709,tonemap=tonemap={algorithm}:desat=0,\
         zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
    )
}

fn crop_filter(
    file: &MediaFile,
    action: &PlannedAction,
    settings: &voom_domain::plan::TranscodeSettings,
) -> Option<String> {
    settings.crop.as_ref()?;
    let detection = file.crop_detection.as_ref()?;
    if detection.rect.is_empty() {
        return None;
    }
    let track = source_video_track(file, action)?;
    let source_width = track.width?;
    let source_height = track.height?;
    let width = source_width
        .checked_sub(detection.rect.left)?
        .checked_sub(detection.rect.right)?;
    let height = source_height
        .checked_sub(detection.rect.top)?
        .checked_sub(detection.rect.bottom)?;
    let width = width - (width % 2);
    let height = height - (height % 2);
    if width == 0 || height == 0 {
        return None;
    }
    Some(format!(
        "crop={width}:{height}:{}:{}",
        detection.rect.left, detection.rect.top
    ))
}

fn requires_software_video_filters(settings: &voom_domain::plan::TranscodeSettings) -> bool {
    let scales_video = settings
        .max_resolution
        .as_deref()
        .and_then(parse_max_height)
        .is_some();
    scales_video || should_tonemap(settings) || settings.crop.is_some()
}

fn hdr_color_primaries(track: &Track) -> &str {
    track.color_primaries.as_deref().unwrap_or("bt2020")
}

fn hdr_color_transfer(track: &Track) -> &str {
    track.color_transfer.as_deref().unwrap_or("smpte2084")
}

fn hdr_color_matrix(track: &Track) -> &str {
    track.color_matrix.as_deref().unwrap_or("bt2020nc")
}

fn x265_hdr_params(track: &Track) -> String {
    let mut params = vec![
        format!("colorprim={}", hdr_color_primaries(track)),
        format!("transfer={}", hdr_color_transfer(track)),
        format!("colormatrix={}", hdr_color_matrix(track)),
    ];
    if let Some(master_display) = &track.master_display {
        params.push(format!("master-display={master_display}"));
    }
    if let (Some(max_cll), Some(max_fall)) = (track.max_cll, track.max_fall) {
        params.push(format!("max-cll={max_cll},{max_fall}"));
    }
    params.join(":")
}

fn apply_hdr_preservation(
    mut cmd: FfmpegCommand,
    encoder: &str,
    codec: &str,
    source_track: &Track,
) -> Result<FfmpegCommand> {
    if !encoder_supports_hdr(codec) {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!(
                "cannot preserve {} HDR metadata while transcoding to {codec}; \
                 choose hevc or av1, or set preserve_hdr: false to tone-map to SDR",
                source_track.hdr_format.as_deref().unwrap_or("source")
            ),
        });
    }

    cmd = cmd.video_color_metadata(
        hdr_color_primaries(source_track),
        hdr_color_transfer(source_track),
        hdr_color_matrix(source_track),
    );

    if codec == "hevc" || encoder.contains("265") || encoder.contains("hevc") {
        cmd = cmd.arg("-pix_fmt");
        let pix_fmt = if encoder.ends_with("_vaapi")
            || encoder.ends_with("_qsv")
            || encoder.ends_with("_nvenc")
        {
            "p010le"
        } else {
            "yuv420p10le"
        };
        cmd = cmd.arg(pix_fmt);
        if encoder == "libx265" {
            cmd = cmd.x265_params(&x265_hdr_params(source_track));
        }
    }

    Ok(cmd)
}

fn apply_sdr_color_metadata(cmd: FfmpegCommand) -> FfmpegCommand {
    cmd.video_color_metadata("bt709", "bt709", "bt709")
}

fn apply_transcode_video(
    mut cmd: FfmpegCommand,
    file: &MediaFile,
    action: &PlannedAction,
    hw_accel: Option<&HwAccelConfig>,
) -> Result<FfmpegCommand> {
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return Ok(cmd);
    };

    let mut owned_config: Option<HwAccelConfig> = None;
    let effective_hw = resolve_effective_hw(settings, hw_accel, &mut owned_config);

    // Check hw_fallback: when false, error if HW encoder isn't available
    let hw_requested = settings.hw.as_deref().is_some_and(|v| v != "none");
    if hw_requested && settings.hw_fallback == Some(false) {
        if let Some(hw_cfg) = effective_hw {
            if !hw_cfg.has_hw_encoder(codec) {
                return Err(VoomError::ToolExecution {
                    tool: "ffmpeg".into(),
                    message: format!(
                        "HW encoder for {codec} not available on \
                         this device and hw_fallback is disabled"
                    ),
                });
            }
        } else {
            return Err(VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!(
                    "no HW acceleration backend available for \
                     {codec} and hw_fallback is disabled"
                ),
            });
        }
    }

    let encoder = if let Some(hw_cfg) = effective_hw {
        hw_cfg.encoder_name(codec)
    } else {
        hwaccel::software_encoder(codec).to_string()
    };

    if let Some(stream) = action.track_index {
        cmd = cmd.video_codec_for_track(stream, &encoder);
    } else {
        cmd = cmd.video_codec(&encoder);
    }

    if let Some(crf_val) = settings.crf {
        cmd = apply_quality(&encoder, cmd, crf_val);
    }

    if let Some(ref preset_val) = settings.preset {
        cmd = apply_preset(&encoder, cmd, preset_val);
    }

    if let Some(ref tune_val) = settings.tune {
        cmd = apply_tune(&encoder, cmd, tune_val);
    }

    if let Some(ref brate) = settings.bitrate {
        cmd = cmd.arg("-b:v").arg(brate);
    }

    if let Some(source_track) = source_video_track(file, action) {
        if should_preserve_hdr(source_track, settings) {
            cmd = apply_hdr_preservation(cmd, &encoder, codec, source_track)?;
        } else if source_track.is_hdr && should_tonemap(settings) {
            cmd = apply_sdr_color_metadata(cmd);
        }
    }

    let filters = collect_video_filters(file, action);
    if !filters.is_empty() {
        cmd = cmd.video_filter(&filters.join(","));
    }

    Ok(cmd)
}

fn apply_transcode_audio(cmd: FfmpegCommand, action: &PlannedAction) -> FfmpegCommand {
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return cmd;
    };

    let resolved = settings
        .channels
        .as_ref()
        .and_then(TranscodeChannels::to_count);
    let encoder = hwaccel::software_encoder(codec).to_string();
    apply_audio_codec_args(
        cmd,
        action.track_index,
        &encoder,
        settings.bitrate.as_deref(),
        resolved,
        settings.loudness.as_ref(),
    )
}

fn apply_synthesize_audio(cmd: FfmpegCommand, action: &PlannedAction) -> FfmpegCommand {
    let ActionParams::Synthesize {
        codec,
        bitrate,
        channels,
        loudness,
        ..
    } = &action.parameters
    else {
        return cmd;
    };

    let codec_str = codec.as_deref().unwrap_or("aac");
    let encoder = hwaccel::software_encoder(codec_str).to_string();
    apply_audio_codec_args(
        cmd,
        action.track_index,
        &encoder,
        bitrate.as_deref(),
        *channels,
        loudness.as_ref(),
    )
}

fn source_video_track<'a>(file: &'a MediaFile, action: &PlannedAction) -> Option<&'a Track> {
    let stream_index = action.track_index?;
    file.tracks
        .iter()
        .find(|track| track.index == stream_index && track.track_type == TrackType::Video)
}

fn pixel_format_allows_direct_hw_decode(pixel_format: Option<&str>) -> bool {
    let Some(format) = pixel_format else {
        return true;
    };
    let lower = format.to_ascii_lowercase();
    !lower.contains("alpha")
        && !lower.contains("yuva")
        && !lower.contains("rgba")
        && !lower.contains("bgra")
        && !lower.contains("gbr")
        && !lower.contains("rgb")
}

fn action_allows_direct_hw_decode(
    file: &MediaFile,
    action: &PlannedAction,
    hw_cfg: &HwAccelConfig,
) -> Vec<String> {
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return Vec::new();
    };
    if !hw_cfg.has_hw_encoder(codec) {
        return Vec::new();
    }
    if requires_software_video_filters(settings) {
        return Vec::new();
    }
    let Some(track) = source_video_track(file, action) else {
        return Vec::new();
    };
    if !pixel_format_allows_direct_hw_decode(track.pixel_format.as_deref()) {
        return Vec::new();
    }
    hw_cfg.decoder_input_args(&track.codec)
}

/// Build an `FFmpeg` command from a plan's actions.
///
/// Groups all actions into a single `FFmpeg` invocation where possible.
pub fn build_ffmpeg_command(
    file: &MediaFile,
    actions: &[&PlannedAction],
    output_path: &Path,
    hw_accel: Option<&HwAccelConfig>,
) -> Result<Vec<String>> {
    let mut cmd = FfmpegCommand::new();

    // Inject device-targeting args (e.g. -vaapi_device, -qsv_device)
    // before the input file so ffmpeg opens the device first.
    if let Some(hw) = hw_accel {
        for arg in hw.device_args() {
            cmd = cmd.arg(&arg);
        }
    }

    // Hardware decode hard-fails if the matching decoder is missing, so
    // only emit it after probing found a same-backend source decoder.
    let mut hw_decode_args = Vec::new();
    for action in actions {
        if action.operation != OperationType::TranscodeVideo {
            continue;
        }
        let ActionParams::Transcode { settings, .. } = &action.parameters else {
            continue;
        };
        let mut owned_config: Option<HwAccelConfig> = None;
        let Some(effective_hw) = resolve_effective_hw(settings, hw_accel, &mut owned_config) else {
            continue;
        };
        hw_decode_args = action_allows_direct_hw_decode(file, action, effective_hw);
        if !hw_decode_args.is_empty() {
            break;
        }
    }

    for arg in hw_decode_args {
        cmd = cmd.arg(&arg);
    }

    cmd = cmd.input(&file.path);
    cmd = cmd.map_all();

    // Start with codec copy for all streams
    cmd = cmd.codec_copy();

    // Process each action
    for action in actions {
        match action.operation {
            OperationType::ConvertContainer => {
                // Container conversion is handled by output extension; codecs stay as copy
            }
            OperationType::TranscodeVideo => {
                cmd = apply_transcode_video(cmd, file, action, hw_accel)?;
            }
            OperationType::TranscodeAudio => {
                cmd = apply_transcode_audio(cmd, action);
            }
            OperationType::SynthesizeAudio => {
                cmd = apply_synthesize_audio(cmd, action);
            }
            OperationType::SetDefault => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "default");
                }
            }
            // Both clear operations use disposition "0" — ffmpeg clears all
            // disposition flags on the stream when set to 0.
            OperationType::ClearDefault | OperationType::ClearForced => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "0");
                }
            }
            OperationType::SetForced => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "forced");
                }
            }
            OperationType::SetTitle => {
                if let Some(stream) = action.track_index {
                    if let ActionParams::Title { title } = &action.parameters {
                        validate_metadata_value(title)?;
                        cmd = cmd.metadata(Some(stream), "title", title);
                    }
                }
            }
            OperationType::SetLanguage => {
                if let Some(stream) = action.track_index {
                    if let ActionParams::Language { language } = &action.parameters {
                        validate_metadata_value(language)?;
                        cmd = cmd.metadata(Some(stream), "language", language);
                    }
                }
            }
            OperationType::SetContainerTag => {
                if let ActionParams::SetTag { tag, value } = &action.parameters {
                    validate_metadata_key(tag)?;
                    validate_metadata_value(value)?;
                    cmd = cmd.metadata(None, tag, value);
                }
            }
            OperationType::ClearContainerTags => {
                if let ActionParams::ClearTags { tags } = &action.parameters {
                    for tag in tags {
                        validate_metadata_key(tag)?;
                        cmd = cmd.clear_metadata(tag);
                    }
                }
            }
            OperationType::DeleteContainerTag => {
                if let ActionParams::DeleteTag { tag } = &action.parameters {
                    validate_metadata_key(tag)?;
                    cmd = cmd.clear_metadata(tag);
                }
            }
            _ => {
                // Other operations (RemoveTrack, ReorderTracks, etc.) not handled by ffmpeg
                tracing::warn!(
                    operation = action.operation.as_str(),
                    "Unsupported operation for FFmpeg executor"
                );
            }
        }
    }

    cmd = cmd.output(output_path);
    Ok(cmd.build())
}

/// Determine the output container extension from the plan's actions.
///
/// If a `ConvertContainer` action is present, uses the target container.
/// Otherwise, preserves the input file's extension.
#[must_use]
pub fn output_extension(file: &MediaFile, actions: &[&PlannedAction]) -> String {
    for action in actions {
        if action.operation == OperationType::ConvertContainer {
            if let ActionParams::Container { container } = &action.parameters {
                return container.as_str().to_string();
            }
        }
    }

    // Preserve the input extension
    match file.container {
        Container::Other => file
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mkv")
            .to_string(),
        _ => file.container.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;
    use voom_domain::media::{Container, CropDetection, CropRect, MediaFile, Track, TrackType};
    use voom_domain::plan::{CropSettings, TranscodeSettings};

    fn sample_mp4_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/video.mp4"));
        file.container = Container::Mp4;
        file.duration = 120.0;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "h264".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
            Track::new(2, TrackType::SubtitleMain, "srt".into()),
        ];
        file
    }

    fn sample_hdr10_file() -> MediaFile {
        let mut file = sample_mp4_file();
        let video = &mut file.tracks[0];
        video.codec = "hevc".into();
        video.is_hdr = true;
        video.hdr_format = Some("HDR10".into());
        video.pixel_format = Some("yuv420p10le".into());
        video.color_primaries = Some("bt2020".into());
        video.color_transfer = Some("smpte2084".into());
        video.color_matrix = Some("bt2020nc".into());
        video.max_cll = Some(1000);
        video.max_fall = Some(400);
        video.master_display =
            Some("G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,1)".into());
        file
    }

    fn sample_avi_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/video.avi"));
        file.container = Container::Avi;
        file.duration = 90.0;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "mpeg4".into()),
            Track::new(1, TrackType::AudioMain, "mp3".into()),
        ];
        file
    }

    fn arg_index(args: &[String], needle: &str) -> usize {
        args.iter()
            .position(|arg| arg == needle)
            .unwrap_or_else(|| panic!("{needle} not found in {args:?}"))
    }

    #[test]
    fn test_build_command_convert_container() {
        let file = sample_avi_file();
        let action = PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container {
                container: Container::Mp4,
            },
            "Convert AVI to MP4",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-y".to_string()));
        assert!(args.contains(&"-hide_banner".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/media/video.avi".to_string()));
        assert!(args.contains(&"-map".to_string()));
        assert!(args.contains(&"0".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"copy".to_string()));
        assert!(args.contains(&"/tmp/output.mp4".to_string()));
    }

    #[test]
    fn test_build_command_transcode_video_crf() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(23))
                    .with_preset(Some("medium".into())),
            },
            "Transcode video to HEVC CRF 23",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-c:v:0".to_string()));
        assert!(args.contains(&"libx265".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"23".to_string()));
        assert!(args.contains(&"-preset".to_string()));
        assert!(args.contains(&"medium".to_string()));
    }

    #[test]
    fn test_build_command_preserves_hdr10_metadata_by_default() {
        let file = sample_hdr10_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crf(Some(23)),
            },
            "Transcode HDR10 video to HEVC",
        );
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &[&action], output, None).unwrap();

        assert_eq!(args[arg_index(&args, "-color_primaries") + 1], "bt2020");
        assert_eq!(args[arg_index(&args, "-color_trc") + 1], "smpte2084");
        assert_eq!(args[arg_index(&args, "-colorspace") + 1], "bt2020nc");
        assert_eq!(args[arg_index(&args, "-pix_fmt") + 1], "yuv420p10le");
        let x265 = &args[arg_index(&args, "-x265-params") + 1];
        assert!(x265.contains("colorprim=bt2020"));
        assert!(x265.contains("transfer=smpte2084"));
        assert!(x265.contains("max-cll=1000,400"));
    }

    #[test]
    fn test_build_command_tonemaps_hdr_to_sdr_when_preserve_disabled() {
        let file = sample_hdr10_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_preserve_hdr(Some(false))
                    .with_tonemap(Some("hable".into())),
            },
            "Tone-map HDR video to SDR",
        );
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &[&action], output, None).unwrap();

        assert_eq!(args[arg_index(&args, "-color_primaries") + 1], "bt709");
        assert_eq!(args[arg_index(&args, "-color_trc") + 1], "bt709");
        assert_eq!(args[arg_index(&args, "-colorspace") + 1], "bt709");
        let filter = &args[arg_index(&args, "-vf") + 1];
        assert!(filter.contains("tonemap=tonemap=hable"));
        assert!(filter.ends_with("format=yuv420p"));
        assert!(!args.contains(&"-x265-params".to_string()));
    }

    #[test]
    fn test_build_command_transcode_video_auto_crop_filter() {
        let mut file = sample_mp4_file();
        file.crop_detection = Some(CropDetection::new(
            CropRect::new(0, 132, 0, 132),
            Utc::now(),
        ));
        file.tracks[0].width = Some(1920);
        file.tracks[0].height = Some(1080);
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crop(Some(CropSettings::auto())),
            },
            "Transcode video to HEVC with auto crop",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        let filter_arg = args
            .iter()
            .position(|arg| arg == "-vf")
            .and_then(|idx| args.get(idx + 1))
            .expect("video filter should be emitted");
        assert_eq!(filter_arg, "crop=1920:816:0:132");
    }

    #[test]
    fn test_build_command_transcode_video_bitrate() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "h264".into(),
                settings: TranscodeSettings::default().with_bitrate(Some("5M".into())),
            },
            "Transcode video to H.264 at 5M",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-c:v:0".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.contains(&"-b:v".to_string()));
        assert!(args.contains(&"5M".to_string()));
    }

    #[test]
    fn test_build_command_transcode_audio() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeAudio,
            1,
            ActionParams::Transcode {
                codec: "opus".into(),
                settings: TranscodeSettings::default()
                    .with_bitrate(Some("128k".into()))
                    .with_channels(Some(TranscodeChannels::Count(2))),
            },
            "Transcode audio to Opus",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-c:a:1".to_string()));
        assert!(args.contains(&"libopus".to_string()));
        assert!(args.contains(&"-b:a".to_string()));
        assert!(args.contains(&"128k".to_string()));
        assert!(args.contains(&"-ac".to_string()));
        assert!(args.contains(&"2".to_string()));
    }

    #[test]
    fn test_build_command_set_metadata() {
        let file = sample_mp4_file();
        let actions_owned = [
            PlannedAction::track_op(
                OperationType::SetTitle,
                1,
                ActionParams::Title {
                    title: "English Stereo".into(),
                },
                "Set track title",
            ),
            PlannedAction::track_op(
                OperationType::SetLanguage,
                1,
                ActionParams::Language {
                    language: "eng".into(),
                },
                "Set track language",
            ),
        ];
        let actions: Vec<&PlannedAction> = actions_owned.iter().collect();
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-metadata:s:1".to_string()));
        assert!(args.contains(&"title=English Stereo".to_string()));
        assert!(args.contains(&"language=eng".to_string()));
    }

    #[test]
    fn test_build_command_set_default() {
        let file = sample_mp4_file();
        let actions_owned = [
            PlannedAction::track_op(
                OperationType::SetDefault,
                1,
                ActionParams::Empty,
                "Set track 1 as default",
            ),
            PlannedAction::track_op(
                OperationType::ClearDefault,
                2,
                ActionParams::Empty,
                "Clear default on track 2",
            ),
        ];
        let actions: Vec<&PlannedAction> = actions_owned.iter().collect();
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-disposition:1".to_string()));
        assert!(args.contains(&"default".to_string()));
        assert!(args.contains(&"-disposition:2".to_string()));
        // Check that "0" is present for clearing
        let disp2_pos = args.iter().position(|a| a == "-disposition:2").unwrap();
        assert_eq!(args[disp2_pos + 1], "0");
    }

    #[test]
    fn test_build_command_set_forced() {
        let file = sample_mp4_file();
        let actions_owned = [
            PlannedAction::track_op(
                OperationType::SetForced,
                2,
                ActionParams::Empty,
                "Set track 2 as forced",
            ),
            PlannedAction::track_op(
                OperationType::ClearForced,
                1,
                ActionParams::Empty,
                "Clear forced on track 1",
            ),
        ];
        let actions: Vec<&PlannedAction> = actions_owned.iter().collect();
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        assert!(args.contains(&"-disposition:2".to_string()));
        assert!(args.contains(&"forced".to_string()));
        assert!(args.contains(&"-disposition:1".to_string()));
        // Check that "0" is present for clearing
        let disp1_pos = args.iter().position(|a| a == "-disposition:1").unwrap();
        assert_eq!(args[disp1_pos + 1], "0");
    }

    #[test]
    fn test_build_command_combined() {
        let file = sample_mp4_file();
        let actions_owned = [
            PlannedAction::track_op(
                OperationType::TranscodeVideo,
                0,
                ActionParams::Transcode {
                    codec: "hevc".into(),
                    settings: TranscodeSettings::default().with_crf(Some(20)),
                },
                "Transcode to HEVC",
            ),
            PlannedAction::track_op(
                OperationType::SetLanguage,
                1,
                ActionParams::Language {
                    language: "eng".into(),
                },
                "Set audio language",
            ),
        ];
        let actions: Vec<&PlannedAction> = actions_owned.iter().collect();
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();

        // Should have both transcode and metadata args
        assert!(args.contains(&"-c:v:0".to_string()));
        assert!(args.contains(&"libx265".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"20".to_string()));
        assert!(args.contains(&"-metadata:s:1".to_string()));
        assert!(args.contains(&"language=eng".to_string()));
    }

    #[test]
    fn test_ffmpeg_command_builder() {
        let cmd = FfmpegCommand::new()
            .input(Path::new("/input.mp4"))
            .map_all()
            .codec_copy()
            .video_codec_for_track(0, "libx265")
            .crf(23)
            .preset("slow")
            .metadata(Some(1), "language", "eng")
            .disposition(1, "default")
            .progress_pipe()
            .output(Path::new("/output.mp4"));

        let args = cmd.build();
        assert_eq!(args[0], "-y");
        assert_eq!(args[1], "-hide_banner");
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"/input.mp4".to_string()));
        assert!(args.contains(&"-map".to_string()));
        assert!(args.contains(&"0".to_string()));
        assert!(args.contains(&"-c".to_string()));
        assert!(args.contains(&"copy".to_string()));
        assert!(args.contains(&"-c:v:0".to_string()));
        assert!(args.contains(&"libx265".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"23".to_string()));
        assert!(args.contains(&"-preset".to_string()));
        assert!(args.contains(&"slow".to_string()));
        assert!(args.contains(&"-metadata:s:1".to_string()));
        assert!(args.contains(&"language=eng".to_string()));
        assert!(args.contains(&"-disposition:1".to_string()));
        assert!(args.contains(&"default".to_string()));
        assert!(args.contains(&"-progress".to_string()));
        assert!(args.contains(&"pipe:1".to_string()));
        // Last arg should be the output
        assert_eq!(args.last().unwrap(), "/output.mp4");
    }

    #[test]
    fn test_output_extension() {
        let file = sample_mp4_file();

        // No convert action — preserve input extension
        let no_actions: Vec<&PlannedAction> = vec![];
        assert_eq!(output_extension(&file, &no_actions), "mp4");

        // Convert container action
        let convert = PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container {
                container: Container::Mkv,
            },
            "Convert to MKV",
        );
        let actions: Vec<&PlannedAction> = vec![&convert];
        assert_eq!(output_extension(&file, &actions), "mkv");

        // AVI file with no conversion
        let avi_file = sample_avi_file();
        assert_eq!(output_extension(&avi_file, &no_actions), "avi");
    }

    #[test]
    fn test_output_extension_webm() {
        let mut file = MediaFile::new(PathBuf::from("/media/video.webm"));
        file.container = Container::Webm;
        let no_actions: Vec<&PlannedAction> = vec![];
        assert_eq!(output_extension(&file, &no_actions), "webm");

        let convert = PlannedAction::file_op(
            OperationType::ConvertContainer,
            ActionParams::Container {
                container: Container::Mp4,
            },
            "Convert to MP4",
        );
        let actions: Vec<&PlannedAction> = vec![&convert];
        assert_eq!(output_extension(&file, &actions), "mp4");
    }

    #[test]
    fn test_build_command_with_vaapi_device() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: Default::default(),
            },
            "Transcode with VAAPI device",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Vaapi)
            .with_device(Some("/dev/dri/renderD129".into()));
        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();

        // -vaapi_device must appear before -i
        let vaapi_pos = args
            .iter()
            .position(|a| a == "-vaapi_device")
            .expect("-vaapi_device not found");
        let input_pos = args.iter().position(|a| a == "-i").expect("-i not found");
        assert!(
            vaapi_pos < input_pos,
            "-vaapi_device ({vaapi_pos}) must come before -i ({input_pos})"
        );
        assert_eq!(args[vaapi_pos + 1], "/dev/dri/renderD129");
    }

    #[test]
    fn test_build_command_with_hw_accel() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crf(Some(23)),
            },
            "Transcode with NVENC",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc);
        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();

        // No -hwaccel flag (HW decode not emitted; encoding-only)
        assert!(!args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"hevc_nvenc".to_string()));
        // NVENC uses -cq, not -crf
        assert!(
            args.contains(&"-cq".to_string()),
            "NVENC should use -cq, got: {args:?}"
        );
        assert!(
            !args.contains(&"-crf".to_string()),
            "NVENC should not use -crf, got: {args:?}"
        );
        assert!(args.contains(&"23".to_string()));
    }

    #[test]
    fn test_build_command_uses_nvenc_hw_decode_when_decoder_available() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crf(Some(23)),
            },
            "Transcode with NVENC",
        );
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["hevc_nvenc".into()])
            .with_hw_decoders(vec!["h264_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert_eq!(args[arg_index(&args, "-hwaccel") + 1], "cuda");
        assert_eq!(args[arg_index(&args, "-hwaccel_output_format") + 1], "cuda");
        assert!(arg_index(&args, "-hwaccel") < arg_index(&args, "-i"));
        assert!(args.contains(&"hevc_nvenc".to_string()));
    }

    #[test]
    fn test_build_command_skips_hw_decode_when_decoder_missing() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default(),
            },
            "Transcode with NVENC",
        );
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["hevc_nvenc".into()])
            .with_hw_decoders(vec!["hevc_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert!(!args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"hevc_nvenc".to_string()));
    }

    #[test]
    fn test_build_command_skips_hw_decode_when_encoder_falls_back_to_software() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default(),
            },
            "Transcode AV1",
        );
        let output = Path::new("/tmp/output.mkv");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()])
            .with_hw_decoders(vec!["h264_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert!(!args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"libsvtav1".to_string()));
    }

    #[test]
    fn test_build_command_skips_hw_decode_when_filters_require_sw_frames() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_max_resolution(Some("720p".into())),
            },
            "Scale and transcode",
        );
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["hevc_nvenc".into()])
            .with_hw_decoders(vec!["h264_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert!(!args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"-vf".to_string()));
    }

    #[test]
    fn test_build_command_allows_hw_decode_when_max_resolution_is_invalid() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_max_resolution(Some("large".into())),
            },
            "Transcode with ignored max resolution",
        );
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["hevc_nvenc".into()])
            .with_hw_decoders(vec!["h264_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert_eq!(args[arg_index(&args, "-hwaccel") + 1], "cuda");
        assert!(!args.contains(&"-vf".to_string()));
    }

    #[test]
    fn test_build_command_skips_hw_decode_for_hdr_tonemap() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hdr_mode(Some("tonemap".into())),
            },
            "Tonemap and transcode",
        );
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["hevc_nvenc".into()])
            .with_hw_decoders(vec!["h264_cuvid".into()]);

        let args = build_ffmpeg_command(&file, &[&action], output, Some(&hw)).unwrap();

        assert!(!args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"-vf".to_string()));
    }

    #[test]
    fn test_build_command_rejects_control_chars_in_title() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::SetTitle,
            1,
            ActionParams::Title {
                title: "Bad\x00Title".into(),
            },
            "Set track title",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let result = build_ffmpeg_command(&file, &actions, output, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_command_rejects_control_chars_in_language() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::SetLanguage,
            1,
            ActionParams::Language {
                language: "en\x01g".into(),
            },
            "Set track language",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let result = build_ffmpeg_command(&file, &actions, output, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_command_set_container_tag() {
        let file = sample_mp4_file();
        let action = PlannedAction::file_op(
            OperationType::SetContainerTag,
            ActionParams::SetTag {
                tag: "title".into(),
                value: "My Movie".into(),
            },
            "Set container tag",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"-metadata".to_string()));
        assert!(args.contains(&"title=My Movie".to_string()));
    }

    #[test]
    fn test_build_command_clear_container_tags() {
        let file = sample_mp4_file();
        let action = PlannedAction::file_op(
            OperationType::ClearContainerTags,
            ActionParams::ClearTags {
                tags: vec!["title".into(), "encoder".into()],
            },
            "Clear all tags",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"title=".to_string()));
        assert!(args.contains(&"encoder=".to_string()));
    }

    #[test]
    fn test_build_command_delete_container_tag() {
        let file = sample_mp4_file();
        let action = PlannedAction::file_op(
            OperationType::DeleteContainerTag,
            ActionParams::DeleteTag {
                tag: "encoder".into(),
            },
            "Delete container tag",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"-metadata".to_string()));
        assert!(args.contains(&"encoder=".to_string()));
    }

    #[test]
    fn test_ffmpeg_clear_metadata_method() {
        let cmd = FfmpegCommand::new()
            .input(Path::new("/input.mp4"))
            .clear_metadata("title")
            .output(Path::new("/output.mp4"));
        let args = cmd.build();
        assert!(args.contains(&"-metadata".to_string()));
        assert!(args.contains(&"title=".to_string()));
    }

    #[test]
    fn test_build_command_hw_none_forces_software() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(23))
                    .with_hw(Some("none".into())),
            },
            "Transcode with software",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        // Even with hw_accel config available, hw: "none" forces software
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc);
        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();
        assert!(
            args.contains(&"libx265".to_string()),
            "hw: none should force software encoder, got: {args:?}"
        );
    }

    #[test]
    fn test_build_command_hw_specific_backend_override() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(23))
                    .with_hw(Some("nvenc".into())),
            },
            "Transcode with NVENC override",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        // No system-wide hw_accel, but per-action hw: nvenc
        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            args.contains(&"hevc_nvenc".to_string()),
            "hw: nvenc should use NVENC encoder, got: {args:?}"
        );
        // No -hwaccel flag (HW decode not emitted)
        assert!(
            !args.contains(&"-hwaccel".to_string()),
            "should not emit -hwaccel: {args:?}"
        );
    }

    #[test]
    fn test_per_action_hw_qsv_no_global() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hw(Some("qsv".into())),
            },
            "Transcode with QSV override",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            !args.contains(&"-hwaccel".to_string()),
            "should not emit -hwaccel: {args:?}"
        );
        assert!(args.contains(&"hevc_qsv".to_string()));
    }

    #[test]
    fn test_global_matches_action_no_duplicate_hwaccel() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hw(Some("nvenc".into())),
            },
            "Transcode with NVENC (matches global)",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc);

        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();
        // No -hwaccel flag (HW decode not emitted)
        assert!(
            !args.contains(&"-hwaccel".to_string()),
            "should not emit -hwaccel: {args:?}"
        );
        assert!(args.contains(&"hevc_nvenc".to_string()));
    }

    #[test]
    fn test_action_hw_none_no_global_no_hwaccel() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hw(Some("none".into())),
            },
            "Transcode software, no global",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            !args.contains(&"-hwaccel".to_string()),
            "hw: none with no global should not emit -hwaccel: {args:?}"
        );
        assert!(args.contains(&"libx265".to_string()));
    }

    #[test]
    fn test_global_hw_preserved_when_action_says_none() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hw(Some("none".into())),
            },
            "Transcode software despite global nvenc",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc);

        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();
        // No -hwaccel flag even with global config (HW decode not emitted)
        assert!(
            !args.contains(&"-hwaccel".to_string()),
            "should not emit -hwaccel: {args:?}"
        );
        // Encoder should be software (hw: none override)
        assert!(args.contains(&"libx265".to_string()));
    }

    #[test]
    fn test_nvenc_uses_cq_not_crf() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(30))
                    .with_preset(Some("slow".into()))
                    .with_hw(Some("nvenc".into())),
            },
            "Transcode AV1 with NVENC",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"av1_nvenc".to_string()));
        assert!(args.contains(&"-cq".to_string()));
        assert!(!args.contains(&"-crf".to_string()));
        assert!(args.contains(&"30".to_string()));
        assert!(args.contains(&"-preset".to_string()));
        assert!(args.contains(&"slow".to_string()));
    }

    #[test]
    fn test_qsv_uses_global_quality() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(25))
                    .with_hw(Some("qsv".into())),
            },
            "Transcode HEVC with QSV",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"hevc_qsv".to_string()));
        assert!(args.contains(&"-global_quality".to_string()));
        assert!(!args.contains(&"-crf".to_string()));
        assert!(args.contains(&"25".to_string()));
    }

    #[test]
    fn test_vaapi_uses_rc_mode_cqp() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "h264".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(20))
                    .with_preset(Some("medium".into()))
                    .with_hw(Some("vaapi".into())),
            },
            "Transcode H.264 with VAAPI",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"h264_vaapi".to_string()));
        assert!(args.contains(&"-rc_mode".to_string()));
        assert!(args.contains(&"CQP".to_string()));
        assert!(args.contains(&"-qp".to_string()));
        assert!(!args.contains(&"-crf".to_string()));
        // VAAPI doesn't support presets — should be omitted
        assert!(
            !args.contains(&"-preset".to_string()),
            "VAAPI should not emit -preset: {args:?}"
        );
    }

    #[test]
    fn test_software_encoder_still_uses_crf() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(28))
                    .with_preset(Some("6".into())),
            },
            "Transcode AV1 software",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"libsvtav1".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"28".to_string()));
    }

    #[test]
    fn test_hw_override_inherits_validated_encoders() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(32))
                    .with_preset(Some("medium".into()))
                    .with_hw(Some("nvenc".into())),
            },
            "Transcode AV1 with NVENC override",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        // System config validated only h264/hevc — av1_nvenc not available
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()]);

        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();
        // Should fall back to software since av1_nvenc not validated
        assert!(
            args.contains(&"libsvtav1".to_string()),
            "should fall back to libsvtav1, got: {args:?}"
        );
        assert!(
            !args.contains(&"av1_nvenc".to_string()),
            "should not use av1_nvenc: {args:?}"
        );
    }

    #[test]
    fn test_hw_fallback_false_errors_when_encoder_unavailable() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(32))
                    .with_hw(Some("nvenc".into()))
                    .with_hw_fallback(Some(false)),
            },
            "Transcode AV1 with NVENC, no fallback",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        // av1_nvenc not in validated list
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()]);

        let result = build_ffmpeg_command(&file, &actions, output, Some(&hw));
        assert!(result.is_err(), "should error when hw_fallback: false");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("hw_fallback"),
            "error should mention hw_fallback: {err}"
        );
    }

    #[test]
    fn test_hw_fallback_false_ok_when_encoder_available() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(23))
                    .with_hw(Some("nvenc".into()))
                    .with_hw_fallback(Some(false)),
            },
            "Transcode HEVC with NVENC, no fallback",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into(), "hevc_nvenc".into()]);

        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();
        assert!(
            args.contains(&"hevc_nvenc".to_string()),
            "should use hevc_nvenc: {args:?}"
        );
    }

    #[test]
    fn test_hw_fallback_false_no_hw_requested_succeeds() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "av1".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(32))
                    .with_hw_fallback(Some(false)),
            },
            "Transcode AV1, no fallback, no HW",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        // hw: None means HW was never requested, so hw_fallback is irrelevant
        let result = build_ffmpeg_command(&file, &actions, output, None);
        assert!(
            result.is_ok(),
            "hw: None should succeed regardless of hw_fallback: {result:?}"
        );
    }

    #[test]
    fn test_hw_fallback_false_hw_requested_but_encoder_unavailable_errors() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(23))
                    .with_hw(Some("nvenc".into()))
                    .with_hw_fallback(Some(false)),
            },
            "Transcode HEVC, nvenc requested, no fallback",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        // System HW validated but encoder NOT in validated list
        let hw = HwAccelConfig::with_backend(crate::hwaccel::HwAccelBackend::Nvenc)
            .with_validated_encoders(vec!["h264_nvenc".into()]);
        let result = build_ffmpeg_command(&file, &actions, output, Some(&hw));
        assert!(
            result.is_err(),
            "hevc_nvenc not in validated list should error when hw_fallback: false"
        );
    }

    #[test]
    fn test_transcode_video_tune() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(20))
                    .with_tune(Some("film".into())),
            },
            "Transcode with tune",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(args.contains(&"-tune".to_string()));
        assert!(args.contains(&"film".to_string()));
    }

    #[test]
    fn test_transcode_video_tune_skipped_for_hw() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_crf(Some(20))
                    .with_tune(Some("film".into()))
                    .with_hw(Some("nvenc".into())),
            },
            "Transcode with tune on NVENC",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            !args.contains(&"-tune".to_string()),
            "NVENC should not emit -tune, got: {args:?}"
        );
    }

    #[test]
    fn test_transcode_video_max_resolution() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_max_resolution(Some("1080p".into())),
            },
            "Transcode with max resolution",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            args.contains(&"-vf".to_string()),
            "should have -vf: {args:?}"
        );
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_pos + 1];
        assert!(
            filter.contains("min(ih,1080)"),
            "should downscale to 1080: {filter}"
        );
        assert!(
            filter.contains("flags=lanczos"),
            "default algorithm should be lanczos: {filter}"
        );
    }

    #[test]
    fn test_transcode_video_max_resolution_with_algorithm() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_max_resolution(Some("720p".into()))
                    .with_scale_algorithm(Some("bicubic".into())),
            },
            "Transcode with scale algorithm",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_pos + 1];
        assert!(
            filter.contains("flags=bicubic"),
            "should use bicubic: {filter}"
        );
    }

    #[test]
    fn test_transcode_video_hdr_tonemap() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hdr_mode(Some("tonemap".into())),
            },
            "Transcode with HDR tonemap",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_pos + 1];
        assert!(
            filter.contains("tonemap=bt2390"),
            "should have tonemap filter: {filter}"
        );
        assert!(
            filter.contains("zscale"),
            "should have zscale filter: {filter}"
        );
    }

    #[test]
    fn test_transcode_video_combined_filters() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default()
                    .with_max_resolution(Some("1080p".into()))
                    .with_hdr_mode(Some("tonemap".into())),
            },
            "Transcode with max res + tonemap",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        // Should have exactly one -vf with both filters combined
        let vf_count = args.iter().filter(|a| *a == "-vf").count();
        assert_eq!(vf_count, 1, "should have exactly one -vf: {args:?}");
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_pos + 1];
        assert!(
            filter.contains("min(ih,1080)"),
            "should have scale filter: {filter}"
        );
        assert!(
            filter.contains("tonemap=bt2390"),
            "should have tonemap filter: {filter}"
        );
    }

    #[test]
    fn test_transcode_video_hdr_preserve_is_noop() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_hdr_mode(Some("preserve".into())),
            },
            "Transcode with HDR preserve",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mkv");

        let args = build_ffmpeg_command(&file, &actions, output, None).unwrap();
        assert!(
            !args.contains(&"-vf".to_string()),
            "preserve should not emit -vf: {args:?}"
        );
    }

    #[test]
    fn test_parse_max_height_values() {
        assert_eq!(parse_max_height("480p"), Some(480));
        assert_eq!(parse_max_height("720p"), Some(720));
        assert_eq!(parse_max_height("1080p"), Some(1080));
        assert_eq!(parse_max_height("1440p"), Some(1440));
        assert_eq!(parse_max_height("2160p"), Some(2160));
        assert_eq!(parse_max_height("4k"), Some(2160));
        assert_eq!(parse_max_height("4K"), Some(2160));
        assert_eq!(parse_max_height("8k"), Some(4320));
        assert_eq!(parse_max_height("bogus"), None);
    }
}
