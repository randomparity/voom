//! `FFmpeg` command builder with safe argument construction.

use std::path::Path;

use voom_domain::errors::Result;
use voom_domain::media::{Container, MediaFile};
use voom_domain::plan::{ActionParams, OperationType, PlannedAction};
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
                let ActionParams::Transcode {
                    codec,
                    crf,
                    preset,
                    bitrate,
                    ..
                } = &action.parameters
                else {
                    continue;
                };

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

                if let Some(crf_val) = crf {
                    cmd = cmd.crf(*crf_val);
                }

                if let Some(preset_val) = preset {
                    cmd = cmd.preset(preset_val);
                }

                if let Some(brate) = bitrate {
                    cmd = cmd.arg("-b:v").arg(brate);
                }
            }
            OperationType::TranscodeAudio => {
                let ActionParams::Transcode {
                    codec,
                    bitrate,
                    channels,
                    ..
                } = &action.parameters
                else {
                    continue;
                };

                let encoder = hwaccel::software_encoder(codec).to_string();

                if let Some(stream) = action.track_index {
                    cmd = cmd.audio_codec_for_track(stream, &encoder);
                } else {
                    cmd = cmd.audio_codec(&encoder);
                }

                if let Some(brate) = bitrate {
                    cmd = cmd.audio_bitrate(brate);
                }

                if let Some(ch) = channels {
                    cmd = cmd.arg("-ac").arg(&ch.to_string());
                }
            }
            OperationType::SynthesizeAudio => {
                let ActionParams::Synthesize {
                    codec,
                    bitrate,
                    channels,
                    ..
                } = &action.parameters
                else {
                    continue;
                };

                let codec_str = codec.as_deref().unwrap_or("aac");
                let bitrate = bitrate.as_deref();
                let channels = *channels;
                let encoder = hwaccel::software_encoder(codec_str).to_string();

                if let Some(stream) = action.track_index {
                    cmd = cmd.audio_codec_for_track(stream, &encoder);
                } else {
                    cmd = cmd.audio_codec(&encoder);
                }

                if let Some(brate) = bitrate {
                    cmd = cmd.audio_bitrate(brate);
                }

                if let Some(ch) = channels {
                    cmd = cmd.arg("-ac").arg(&ch.to_string());
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
            OperationType::SetForced => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "forced");
                }
            }
            OperationType::ClearForced => {
                if let Some(stream) = action.track_index {
                    cmd = cmd.disposition(stream, "0");
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
                crf: Some(23),
                preset: Some("medium".into()),
                bitrate: None,
                channels: None,
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
    fn test_build_command_transcode_video_bitrate() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "h264".into(),
                crf: None,
                preset: None,
                bitrate: Some("5M".into()),
                channels: None,
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
                crf: None,
                preset: None,
                bitrate: Some("128k".into()),
                channels: Some(2),
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
                    crf: Some(20),
                    preset: None,
                    bitrate: None,
                    channels: None,
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
    fn test_build_command_with_hw_accel() {
        let file = sample_mp4_file();
        let action = PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                crf: Some(23),
                preset: None,
                bitrate: None,
                channels: None,
            },
            "Transcode with NVENC",
        );
        let actions: Vec<&PlannedAction> = vec![&action];
        let output = Path::new("/tmp/output.mp4");

        let hw = HwAccelConfig {
            backend: Some(crate::hwaccel::HwAccelBackend::Nvenc),
        };
        let args = build_ffmpeg_command(&file, &actions, output, Some(&hw)).unwrap();

        assert!(args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"cuda".to_string()));
        assert!(args.contains(&"hevc_nvenc".to_string()));
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
}
