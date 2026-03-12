//! FFmpeg command builder with safe argument construction.

use std::path::Path;

use voom_domain::errors::Result;
use voom_domain::media::{Container, MediaFile};
use voom_domain::plan::{OperationType, PlannedAction};
use voom_domain::utils::sanitize::validate_metadata_value;

use crate::hwaccel::{self, HwAccelConfig};

/// A builder for constructing FFmpeg command-line arguments.
#[derive(Debug, Clone)]
pub struct FfmpegCommand {
    args: Vec<String>,
}

impl FfmpegCommand {
    /// Create a new command with default flags (`-y -hide_banner`).
    pub fn new() -> Self {
        Self {
            args: vec!["-y".to_string(), "-hide_banner".to_string()],
        }
    }

    /// Add an input file.
    pub fn input(mut self, path: &Path) -> Self {
        self.args.push("-i".to_string());
        self.args.push(path.to_string_lossy().to_string());
        self
    }

    /// Set the output file (must be called last before `build`).
    pub fn output(mut self, path: &Path) -> Self {
        self.args.push(path.to_string_lossy().to_string());
        self
    }

    /// Map all streams from input (`-map 0`).
    pub fn map_all(mut self) -> Self {
        self.args.push("-map".to_string());
        self.args.push("0".to_string());
        self
    }

    /// Map a specific track by index (`-map 0:{index}`).
    pub fn map_track(mut self, index: u32) -> Self {
        self.args.push("-map".to_string());
        self.args.push(format!("0:{index}"));
        self
    }

    /// Copy all codecs (`-c copy`).
    pub fn codec_copy(mut self) -> Self {
        self.args.push("-c".to_string());
        self.args.push("copy".to_string());
        self
    }

    /// Set global video codec (`-c:v {codec}`).
    pub fn video_codec(mut self, codec: &str) -> Self {
        self.args.push("-c:v".to_string());
        self.args.push(codec.to_string());
        self
    }

    /// Set global audio codec (`-c:a {codec}`).
    pub fn audio_codec(mut self, codec: &str) -> Self {
        self.args.push("-c:a".to_string());
        self.args.push(codec.to_string());
        self
    }

    /// Set video codec for a specific stream (`-c:v:{stream} {codec}`).
    pub fn video_codec_for_track(mut self, stream: u32, codec: &str) -> Self {
        self.args.push(format!("-c:v:{stream}"));
        self.args.push(codec.to_string());
        self
    }

    /// Set audio codec for a specific stream (`-c:a:{stream} {codec}`).
    pub fn audio_codec_for_track(mut self, stream: u32, codec: &str) -> Self {
        self.args.push(format!("-c:a:{stream}"));
        self.args.push(codec.to_string());
        self
    }

    /// Set CRF value (`-crf {value}`).
    pub fn crf(mut self, value: u32) -> Self {
        self.args.push("-crf".to_string());
        self.args.push(value.to_string());
        self
    }

    /// Set audio bitrate (`-b:a {bitrate}`).
    pub fn audio_bitrate(mut self, bitrate: &str) -> Self {
        self.args.push("-b:a".to_string());
        self.args.push(bitrate.to_string());
        self
    }

    /// Set encoding preset (`-preset {preset}`).
    pub fn preset(mut self, preset: &str) -> Self {
        self.args.push("-preset".to_string());
        self.args.push(preset.to_string());
        self
    }

    /// Set metadata on a stream or globally.
    ///
    /// With `stream_index`: `-metadata:s:{index} {key}={value}`
    /// Without: `-metadata {key}={value}`
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
    pub fn disposition(mut self, stream_index: u32, value: &str) -> Self {
        self.args.push(format!("-disposition:{stream_index}"));
        self.args.push(value.to_string());
        self
    }

    /// Enable progress output to pipe (`-progress pipe:1`).
    pub fn progress_pipe(mut self) -> Self {
        self.args.push("-progress".to_string());
        self.args.push("pipe:1".to_string());
        self
    }

    /// Add a raw argument.
    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

    /// Consume the builder and return the argument list.
    pub fn build(self) -> Vec<String> {
        self.args
    }
}

impl Default for FfmpegCommand {
    fn default() -> Self {
        Self::new()
    }
}

/// Build an FFmpeg command from a plan's actions.
///
/// Groups all actions into a single FFmpeg invocation where possible.
pub fn build_ffmpeg_command(
    file: &MediaFile,
    actions: &[&PlannedAction],
    output_path: &Path,
    hw_accel: Option<&HwAccelConfig>,
) -> Result<Vec<String>> {
    let mut cmd = FfmpegCommand::new();

    // Add HW accel input args if provided
    if let Some(hw) = hw_accel {
        for arg in hw.input_args() {
            cmd = cmd.arg(&arg);
        }
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
                let codec = action
                    .parameters
                    .get("codec")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hevc");

                let encoder = if let Some(hw) = hw_accel {
                    hw.encoder_name(codec)
                } else {
                    hwaccel::software_encoder(codec).to_string()
                };

                if let Some(stream) = action.track_index {
                    cmd = cmd.video_codec_for_track(stream, &encoder);
                } else {
                    cmd = cmd.video_codec(&encoder);
                }

                if let Some(crf_val) = action.parameters.get("crf").and_then(|v| v.as_u64()) {
                    cmd = cmd.crf(crf_val as u32);
                }

                if let Some(preset_val) = action.parameters.get("preset").and_then(|v| v.as_str()) {
                    cmd = cmd.preset(preset_val);
                }

                if let Some(bitrate) = action.parameters.get("bitrate").and_then(|v| v.as_str()) {
                    cmd = cmd.arg("-b:v").arg(bitrate);
                }
            }
            OperationType::TranscodeAudio | OperationType::SynthesizeAudio => {
                let codec = action
                    .parameters
                    .get("codec")
                    .and_then(|v| v.as_str())
                    .unwrap_or("aac");

                let encoder = hwaccel::software_encoder(codec).to_string();

                if let Some(stream) = action.track_index {
                    cmd = cmd.audio_codec_for_track(stream, &encoder);
                } else {
                    cmd = cmd.audio_codec(&encoder);
                }

                if let Some(bitrate) = action.parameters.get("bitrate").and_then(|v| v.as_str()) {
                    cmd = cmd.audio_bitrate(bitrate);
                }

                if let Some(channels) = action.parameters.get("channels").and_then(|v| v.as_u64()) {
                    cmd = cmd.arg("-ac").arg(&channels.to_string());
                }
            }
            OperationType::SetDefault => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "default");
                }
            }
            OperationType::ClearDefault => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "0");
                }
            }
            OperationType::SetTitle => {
                if let Some(stream) = action.track_index {
                    if let Some(title) = action.parameters.get("title").and_then(|v| v.as_str()) {
                        validate_metadata_value(title)?;
                        cmd = cmd.metadata(Some(stream), "title", title);
                    }
                }
            }
            OperationType::SetLanguage => {
                if let Some(stream) = action.track_index {
                    if let Some(lang) = action.parameters.get("language").and_then(|v| v.as_str()) {
                        validate_metadata_value(lang)?;
                        cmd = cmd.metadata(Some(stream), "language", lang);
                    }
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
pub fn output_extension(file: &MediaFile, actions: &[&PlannedAction]) -> String {
    for action in actions {
        if action.operation == OperationType::ConvertContainer {
            if let Some(container) = action.parameters.get("container").and_then(|v| v.as_str()) {
                return container.to_string();
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
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

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

    #[test]
    fn test_build_command_convert_container() {
        let file = sample_avi_file();
        let action = PlannedAction {
            operation: OperationType::ConvertContainer,
            track_index: None,
            parameters: serde_json::json!({"container": "mp4"}),
            description: "Convert AVI to MP4".into(),
        };
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
        let action = PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: serde_json::json!({"codec": "hevc", "crf": 23, "preset": "medium"}),
            description: "Transcode video to HEVC CRF 23".into(),
        };
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
    fn test_build_command_transcode_video_bitrate() {
        let file = sample_mp4_file();
        let action = PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: serde_json::json!({"codec": "h264", "bitrate": "5M"}),
            description: "Transcode video to H.264 at 5M".into(),
        };
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
        let action = PlannedAction {
            operation: OperationType::TranscodeAudio,
            track_index: Some(1),
            parameters: serde_json::json!({"codec": "opus", "bitrate": "128k", "channels": 2}),
            description: "Transcode audio to Opus".into(),
        };
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
        let actions_owned = vec![
            PlannedAction {
                operation: OperationType::SetTitle,
                track_index: Some(1),
                parameters: serde_json::json!({"title": "English Stereo"}),
                description: "Set track title".into(),
            },
            PlannedAction {
                operation: OperationType::SetLanguage,
                track_index: Some(1),
                parameters: serde_json::json!({"language": "eng"}),
                description: "Set track language".into(),
            },
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
        let actions_owned = vec![
            PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set track 1 as default".into(),
            },
            PlannedAction {
                operation: OperationType::ClearDefault,
                track_index: Some(2),
                parameters: serde_json::json!({}),
                description: "Clear default on track 2".into(),
            },
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
    fn test_build_command_combined() {
        let file = sample_mp4_file();
        let actions_owned = vec![
            PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc", "crf": 20}),
                description: "Transcode to HEVC".into(),
            },
            PlannedAction {
                operation: OperationType::SetLanguage,
                track_index: Some(1),
                parameters: serde_json::json!({"language": "eng"}),
                description: "Set audio language".into(),
            },
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
        let convert = PlannedAction {
            operation: OperationType::ConvertContainer,
            track_index: None,
            parameters: serde_json::json!({"container": "mkv"}),
            description: "Convert to MKV".into(),
        };
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

        let convert = PlannedAction {
            operation: OperationType::ConvertContainer,
            track_index: None,
            parameters: serde_json::json!({"container": "mp4"}),
            description: "Convert to MP4".into(),
        };
        let actions: Vec<&PlannedAction> = vec![&convert];
        assert_eq!(output_extension(&file, &actions), "mp4");
    }

    #[test]
    fn test_build_command_with_hw_accel() {
        let file = sample_mp4_file();
        let action = PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: serde_json::json!({"codec": "hevc", "crf": 23}),
            description: "Transcode with NVENC".into(),
        };
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let hw = HwAccelConfig {
            backend: Some(crate::hwaccel::HwAccelBackend::Nvenc),
            enabled: true,
        };
        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();

        assert!(args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"cuda".to_string()));
        assert!(args.contains(&"hevc_nvenc".to_string()));
    }

    #[test]
    fn test_build_command_rejects_control_chars_in_title() {
        let file = sample_mp4_file();
        let action = PlannedAction {
            operation: OperationType::SetTitle,
            track_index: Some(1),
            parameters: serde_json::json!({"title": "Bad\x00Title"}),
            description: "Set track title".into(),
        };
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let result = build_ffmpeg_command(&file, &actions, output, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_command_rejects_control_chars_in_language() {
        let file = sample_mp4_file();
        let action = PlannedAction {
            operation: OperationType::SetLanguage,
            track_index: Some(1),
            parameters: serde_json::json!({"language": "en\x01g"}),
            description: "Set track language".into(),
        };
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let result = build_ffmpeg_command(&file, &actions, output, None);
        assert!(result.is_err());
    }
}
