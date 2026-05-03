use std::collections::HashMap;
use std::path::Path;

use voom_domain::errors::Result;
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::utils::codecs::normalize_codec;
use voom_domain::utils::language::normalize_language;

/// Parse ffprobe JSON output into a `MediaFile`.
pub fn parse_ffprobe_output(
    json: &serde_json::Value,
    path: &Path,
    size: u64,
    content_hash: Option<&str>,
) -> Result<MediaFile> {
    let container = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map_or(Container::Other, Container::from_extension);

    let format = json.get("format").unwrap_or(&serde_json::Value::Null);
    let duration = parse_duration(format);
    let bitrate = parse_bitrate(format);
    let tags = parse_format_tags(format);

    let empty_streams = Vec::new();
    let streams = json
        .get("streams")
        .and_then(|s| s.as_array())
        .unwrap_or(&empty_streams);

    let tracks = parse_streams(streams);

    let mut mf = MediaFile::new(path.to_path_buf());
    mf.size = size;
    // Caller guarantees None for absent hashes; no empty-string sentinel at this layer.
    mf.content_hash = content_hash.map(str::to_string);
    mf.container = container;
    mf.duration = duration;
    mf.bitrate = bitrate;
    mf.tracks = tracks;
    mf.tags = tags;
    Ok(mf)
}

fn parse_duration(format: &serde_json::Value) -> f64 {
    format
        .get("duration")
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn parse_bitrate(format: &serde_json::Value) -> Option<u32> {
    format
        .get("bit_rate")
        .and_then(|b| b.as_str())
        .and_then(|s| s.parse::<u32>().ok())
}

fn parse_format_tags(format: &serde_json::Value) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    if let Some(tag_obj) = format.get("tags").and_then(|t| t.as_object()) {
        for (key, val) in tag_obj {
            if let Some(s) = val.as_str() {
                tags.insert(key.to_lowercase(), s.to_string());
            }
        }
    }
    tags
}

fn parse_streams(streams: &[serde_json::Value]) -> Vec<Track> {
    streams
        .iter()
        .enumerate()
        .filter_map(|(idx, stream)| {
            let index = u32::try_from(idx).unwrap_or(u32::MAX);
            parse_stream(index, stream)
        })
        .collect()
}

/// Codecs that are image-only in a film library context. A track using one of
/// these codecs is treated as cover art / thumbnail when it lacks evidence of
/// real motion (frame count > 1 AND non-zero duration).
const IMAGE_ONLY_CODECS: &[&str] = &["mjpeg", "png", "bmp", "gif", "webp"];

/// Returns true when a `codec_type=video` stream looks like an embedded cover
/// image rather than primary motion video.
///
/// Heuristic (codec must be in `IMAGE_ONLY_CODECS`, then any of):
/// - `nb_frames` parses to a value `<= 1`
/// - `nb_frames` is absent AND `duration` is missing/`N/A`/parses to `0.0`
///
/// `codec` is the *normalized* codec name (matches the value stored on `Track.codec`).
fn is_likely_cover_art_stream(codec: &str, stream: &serde_json::Value) -> bool {
    if !IMAGE_ONLY_CODECS.contains(&codec) {
        return false;
    }

    let nb_frames = stream
        .get("nb_frames")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());

    if let Some(frames) = nb_frames {
        return frames <= 1;
    }

    let duration = stream
        .get("duration")
        .and_then(|v| v.as_str())
        .filter(|s| *s != "N/A")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    duration <= 0.0
}

#[allow(clippy::too_many_lines)] // Single match over codec_type; splitting would scatter field logic.
fn parse_stream(index: u32, stream: &serde_json::Value) -> Option<Track> {
    let codec_type = stream.get("codec_type")?.as_str()?;
    let codec_name = stream
        .get("codec_name")
        .and_then(|c| c.as_str())
        .unwrap_or("unknown");

    // Normalize the codec name if possible, otherwise use raw name
    let codec = normalize_codec(codec_name).map_or_else(
        || codec_name.to_lowercase(),
        std::string::ToString::to_string,
    );

    let language = stream
        .get("tags")
        .and_then(|t| t.get("language"))
        .and_then(|l| l.as_str())
        .and_then(normalize_language)
        .unwrap_or("und")
        .to_string();

    let title = stream
        .get("tags")
        .and_then(|t| t.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let disposition = stream
        .get("disposition")
        .unwrap_or(&serde_json::Value::Null);
    let disp_flag = |key| {
        disposition
            .get(key)
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|v| v == 1)
    };
    let is_default = disp_flag("default");
    let is_forced = disp_flag("forced");
    let is_commentary = disp_flag("comment");

    // Common fields shared across all track types
    let mut common = Track::new(index, TrackType::Video, codec);
    common.language = language;
    common.title = title;
    common.is_default = is_default;
    common.is_forced = is_forced;

    match codec_type {
        "video" => {
            // Skip attached pictures (album art)
            if disp_flag("attached_pic") {
                common.track_type = TrackType::Attachment;
                return Some(common);
            }

            // Heuristic: image codecs with no real motion are cover art even
            // when the muxer didn't set the attached_pic disposition flag.
            // See issue #156.
            if is_likely_cover_art_stream(&common.codec, stream) {
                common.track_type = TrackType::Attachment;
                return Some(common);
            }

            let width = stream
                .get("width")
                .and_then(serde_json::Value::as_u64)
                .map(|w| u32::try_from(w).unwrap_or(u32::MAX));
            let height = stream
                .get("height")
                .and_then(serde_json::Value::as_u64)
                .map(|h| u32::try_from(h).unwrap_or(u32::MAX));
            let frame_rate = parse_frame_rate(stream);
            let is_vfr = detect_vfr(stream);
            let (is_hdr, hdr_format) = detect_hdr(stream);
            let pixel_format = stream
                .get("pix_fmt")
                .and_then(|p| p.as_str())
                .map(std::string::ToString::to_string);

            common.track_type = TrackType::Video;
            common.width = width;
            common.height = height;
            common.frame_rate = frame_rate;
            common.is_vfr = is_vfr;
            common.is_hdr = is_hdr;
            common.hdr_format = hdr_format;
            common.pixel_format = pixel_format;
            Some(common)
        }
        "audio" => {
            let channels = stream
                .get("channels")
                .and_then(serde_json::Value::as_u64)
                .map(|c| u32::try_from(c).unwrap_or(u32::MAX));
            let channel_layout = stream
                .get("channel_layout")
                .and_then(|c| c.as_str())
                .map(std::string::ToString::to_string);
            let sample_rate = stream
                .get("sample_rate")
                .and_then(|s| s.as_str())
                .and_then(|s| s.parse::<u32>().ok());
            let bit_depth = stream
                .get("bits_per_raw_sample")
                .and_then(|b| b.as_str())
                .and_then(|s| s.parse::<u32>().ok());

            let track_type =
                classify_audio_track(&common.title, is_default, is_commentary, is_forced);

            common.track_type = track_type;
            common.channels = channels;
            common.channel_layout = channel_layout;
            common.sample_rate = sample_rate;
            common.bit_depth = bit_depth;
            Some(common)
        }
        "subtitle" => {
            let track_type =
                classify_subtitle_track(&common.title, is_default, is_commentary, is_forced);

            common.track_type = track_type;
            Some(common)
        }
        "attachment" => {
            common.track_type = TrackType::Attachment;
            common.is_default = false;
            common.is_forced = false;
            Some(common)
        }
        _ => None,
    }
}

/// Parse frame rate from `r_frame_rate` (e.g., "24000/1001").
fn parse_frame_rate(stream: &serde_json::Value) -> Option<f64> {
    parse_fraction(stream.get("r_frame_rate").and_then(|v| v.as_str()))
}

fn detect_vfr(stream: &serde_json::Value) -> bool {
    let r_rate = parse_fraction(stream.get("r_frame_rate").and_then(|v| v.as_str()));
    let avg_rate = parse_fraction(stream.get("avg_frame_rate").and_then(|v| v.as_str()));

    match (r_rate, avg_rate) {
        (Some(r), Some(avg)) if r > 0.0 && avg > 0.0 => {
            let diff = (r - avg).abs() / r;
            diff > 0.01 // More than 1% difference suggests VFR
        }
        _ => false,
    }
}

/// Parse a fraction string like "24000/1001" into a float.
fn parse_fraction(s: Option<&str>) -> Option<f64> {
    let s = s?;
    if let Some((num, den)) = s.split_once('/') {
        let num: f64 = num.parse().ok()?;
        let den: f64 = den.parse().ok()?;
        if den > 0.0 {
            return Some(num / den);
        }
    }
    s.parse().ok()
}

fn detect_hdr(stream: &serde_json::Value) -> (bool, Option<String>) {
    // Check color transfer characteristics
    let color_transfer = stream
        .get("color_transfer")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    let is_hdr_transfer = matches!(
        color_transfer,
        "smpte2084" | "arib-std-b67" | "smpte428" | "bt2020-10" | "bt2020-12"
    );

    // Check side data for HDR metadata (single pass)
    let empty_side_data = Vec::new();
    let side_data = stream
        .get("side_data_list")
        .and_then(|s| s.as_array())
        .unwrap_or(&empty_side_data);

    let mut has_hdr_side_data = false;
    let mut has_dovi = false;
    for sd in side_data {
        let side_type = sd
            .get("side_data_type")
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if side_type.contains("DOVI") || side_type.contains("Dolby Vision") {
            has_dovi = true;
            has_hdr_side_data = true;
        } else if side_type.contains("Mastering display")
            || side_type.contains("Content light level")
        {
            has_hdr_side_data = true;
        }
    }

    let is_hdr = is_hdr_transfer || has_hdr_side_data;

    let hdr_format = if has_dovi {
        Some("Dolby Vision".to_string())
    } else if color_transfer == "smpte2084" {
        Some("HDR10".to_string())
    } else if color_transfer == "arib-std-b67" {
        Some("HLG".to_string())
    } else if has_hdr_side_data {
        Some("HDR10".to_string())
    } else {
        None
    };

    (is_hdr, hdr_format)
}

fn classify_audio_track(
    title: &str,
    is_default: bool,
    is_commentary: bool,
    _is_forced: bool,
) -> TrackType {
    let title_lower = title.to_lowercase();

    if is_commentary || title_lower.contains("commentary") || title_lower.contains("comment") {
        return TrackType::AudioCommentary;
    }
    if title_lower.contains("music") || title_lower.contains("soundtrack") {
        return TrackType::AudioMusic;
    }
    if title_lower.contains("effect") || title_lower.contains("sfx") {
        return TrackType::AudioSfx;
    }

    if is_default {
        TrackType::AudioMain
    } else {
        TrackType::AudioAlternate
    }
}

fn classify_subtitle_track(
    title: &str,
    _is_default: bool,
    is_commentary: bool,
    is_forced: bool,
) -> TrackType {
    if is_forced {
        return TrackType::SubtitleForced;
    }

    let title_lower = title.to_lowercase();

    if is_commentary || title_lower.contains("commentary") || title_lower.contains("comment") {
        return TrackType::SubtitleCommentary;
    }

    TrackType::SubtitleMain
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // tests compare exact-representable literals and default 0.0
mod tests {
    use super::*;

    fn make_ffprobe_json(streams: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({
            "format": {
                "duration": "120.500",
                "bit_rate": "5000000",
                "tags": {
                    "title": "Test Movie",
                    "ENCODER": "libmkv"
                }
            },
            "streams": streams
        })
    }

    fn video_stream() -> serde_json::Value {
        serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "hevc",
            "width": 1920,
            "height": 1080,
            "r_frame_rate": "24000/1001",
            "avg_frame_rate": "24000/1001",
            "pix_fmt": "yuv420p10le",
            "color_transfer": "smpte2084",
            "disposition": {"default": 1, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {"language": "und"},
            "side_data_list": [
                {"side_data_type": "Mastering display metadata"}
            ]
        })
    }

    fn audio_stream(lang: &str, is_default: bool) -> serde_json::Value {
        serde_json::json!({
            "index": 1,
            "codec_type": "audio",
            "codec_name": "aac",
            "channels": 6,
            "channel_layout": "5.1",
            "sample_rate": "48000",
            "bits_per_raw_sample": "24",
            "disposition": {
                "default": i32::from(is_default),
                "forced": 0,
                "comment": 0
            },
            "tags": {"language": lang, "title": "Surround Sound"}
        })
    }

    fn subtitle_stream(lang: &str, forced: bool) -> serde_json::Value {
        serde_json::json!({
            "index": 3,
            "codec_type": "subtitle",
            "codec_name": "subrip",
            "disposition": {
                "default": 0,
                "forced": i32::from(forced),
                "comment": 0
            },
            "tags": {"language": lang, "title": ""}
        })
    }

    #[test]
    fn test_parse_complete_file() {
        let json = make_ffprobe_json(&[
            video_stream(),
            audio_stream("eng", true),
            subtitle_stream("eng", false),
        ]);

        let file = parse_ffprobe_output(
            &json,
            Path::new("/test/movie.mkv"),
            1_000_000,
            Some("abc123"),
        )
        .unwrap();

        assert_eq!(file.path, Path::new("/test/movie.mkv"));
        assert_eq!(file.size, 1_000_000);
        assert_eq!(file.content_hash, Some("abc123".to_string()));
        assert_eq!(file.container, Container::Mkv);
        assert!((file.duration - 120.5).abs() < 0.01);
        assert_eq!(file.bitrate, Some(5_000_000));
        assert_eq!(file.tracks.len(), 3);
    }

    #[test]
    fn test_parse_video_track() {
        let json = make_ffprobe_json(&[video_stream()]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        let track = &file.tracks[0];
        assert_eq!(track.track_type, TrackType::Video);
        assert_eq!(track.codec, "hevc");
        assert_eq!(track.width, Some(1920));
        assert_eq!(track.height, Some(1080));
        assert!(track.frame_rate.is_some());
        assert!((track.frame_rate.unwrap() - 23.976).abs() < 0.01);
        assert!(track.is_hdr);
        assert_eq!(track.hdr_format.as_deref(), Some("HDR10"));
        assert_eq!(track.pixel_format.as_deref(), Some("yuv420p10le"));
    }

    #[test]
    fn test_parse_audio_track() {
        let json = make_ffprobe_json(&[audio_stream("eng", true)]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        let track = &file.tracks[0];
        assert_eq!(track.track_type, TrackType::AudioMain);
        assert_eq!(track.codec, "aac");
        assert_eq!(track.language, "eng");
        assert_eq!(track.channels, Some(6));
        assert_eq!(track.channel_layout.as_deref(), Some("5.1"));
        assert_eq!(track.sample_rate, Some(48000));
        assert_eq!(track.bit_depth, Some(24));
        assert!(track.is_default);
    }

    #[test]
    fn test_parse_subtitle_track() {
        let json = make_ffprobe_json(&[subtitle_stream("eng", false)]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        let track = &file.tracks[0];
        assert_eq!(track.track_type, TrackType::SubtitleMain);
        assert_eq!(track.codec, "subrip");
        assert_eq!(track.language, "eng");
    }

    #[test]
    fn test_parse_forced_subtitle() {
        let json = make_ffprobe_json(&[subtitle_stream("eng", true)]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        assert_eq!(file.tracks[0].track_type, TrackType::SubtitleForced);
        assert!(file.tracks[0].is_forced);
    }

    #[test]
    fn test_classify_commentary_audio() {
        let stream = serde_json::json!({
            "index": 2,
            "codec_type": "audio",
            "codec_name": "aac",
            "channels": 2,
            "sample_rate": "48000",
            "disposition": {"default": 0, "forced": 0, "comment": 1},
            "tags": {"language": "eng", "title": "Director's Commentary"}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        assert_eq!(file.tracks[0].track_type, TrackType::AudioCommentary);
    }

    #[test]
    fn test_codec_normalization() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "h265",
            "width": 1920,
            "height": 1080,
            "r_frame_rate": "24/1",
            "avg_frame_rate": "24/1",
            "disposition": {"default": 1, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        // h265 should be normalized to hevc
        assert_eq!(file.tracks[0].codec, "hevc");
    }

    #[test]
    fn test_language_normalization() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "audio",
            "codec_name": "aac",
            "channels": 2,
            "sample_rate": "48000",
            "disposition": {"default": 1, "forced": 0, "comment": 0},
            "tags": {"language": "ja"}  // 2-letter code
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        // "ja" should be normalized to "jpn"
        assert_eq!(file.tracks[0].language, "jpn");
    }

    #[test]
    fn test_parse_format_tags() {
        let json = make_ffprobe_json(&[]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        assert_eq!(
            file.tags.get("title").map(std::string::String::as_str),
            Some("Test Movie")
        );
        assert_eq!(
            file.tags.get("encoder").map(std::string::String::as_str),
            Some("libmkv")
        );
    }

    #[test]
    fn test_parse_empty_streams() {
        let json = serde_json::json!({"format": {}, "streams": []});
        let file = parse_ffprobe_output(&json, Path::new("/test.mp4"), 500, Some("hash")).unwrap();

        assert!(file.tracks.is_empty());
        assert_eq!(file.container, Container::Mp4);
        assert_eq!(file.duration, 0.0);
        assert_eq!(file.bitrate, None);
    }

    #[test]
    fn test_detect_vfr() {
        let cfr_stream = serde_json::json!({
            "r_frame_rate": "24000/1001",
            "avg_frame_rate": "24000/1001"
        });
        assert!(!super::detect_vfr(&cfr_stream));

        let vfr_stream = serde_json::json!({
            "r_frame_rate": "30/1",
            "avg_frame_rate": "24000/1001"
        });
        assert!(super::detect_vfr(&vfr_stream));
    }

    #[test]
    fn test_detect_hdr_smpte2084() {
        let stream = serde_json::json!({"color_transfer": "smpte2084"});
        let (is_hdr, format) = detect_hdr(&stream);
        assert!(is_hdr);
        assert_eq!(format.as_deref(), Some("HDR10"));
    }

    #[test]
    fn test_detect_hdr_hlg() {
        let stream = serde_json::json!({"color_transfer": "arib-std-b67"});
        let (is_hdr, format) = detect_hdr(&stream);
        assert!(is_hdr);
        assert_eq!(format.as_deref(), Some("HLG"));
    }

    #[test]
    fn test_detect_dolby_vision() {
        let stream = serde_json::json!({
            "color_transfer": "smpte2084",
            "side_data_list": [
                {"side_data_type": "DOVI configuration record"}
            ]
        });
        let (is_hdr, format) = detect_hdr(&stream);
        assert!(is_hdr);
        assert_eq!(format.as_deref(), Some("Dolby Vision"));
    }

    #[test]
    fn test_detect_sdr() {
        let stream = serde_json::json!({"color_transfer": "bt709"});
        let (is_hdr, format) = detect_hdr(&stream);
        assert!(!is_hdr);
        assert!(format.is_none());
    }

    #[test]
    fn test_attached_pic_becomes_attachment() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "mjpeg",
            "width": 600,
            "height": 600,
            "r_frame_rate": "0/0",
            "avg_frame_rate": "0/0",
            "disposition": {"default": 0, "forced": 0, "attached_pic": 1, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mp4"), 0, None).unwrap();

        assert_eq!(file.tracks[0].track_type, TrackType::Attachment);
    }

    #[test]
    fn test_non_default_audio_is_alternate() {
        let json = make_ffprobe_json(&[audio_stream("jpn", false)]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();

        assert_eq!(file.tracks[0].track_type, TrackType::AudioAlternate);
    }

    #[test]
    fn test_parse_frame_rate_fraction() {
        let stream = serde_json::json!({"r_frame_rate": "24000/1001"});
        let rate = parse_frame_rate(&stream).unwrap();
        assert!((rate - 23.976).abs() < 0.01);
    }

    #[test]
    fn test_parse_frame_rate_integer() {
        let stream = serde_json::json!({"r_frame_rate": "25/1"});
        let rate = parse_frame_rate(&stream).unwrap();
        assert!((rate - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_mjpeg_with_single_frame_classified_as_attachment() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "mjpeg",
            "width": 600,
            "height": 600,
            "r_frame_rate": "0/0",
            "avg_frame_rate": "0/0",
            "nb_frames": "1",
            "disposition": {"default": 0, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();
        assert_eq!(file.tracks[0].track_type, TrackType::Attachment);
    }

    #[test]
    fn test_png_cover_without_frame_count_classified_as_attachment() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "png",
            "width": 1000,
            "height": 1500,
            "r_frame_rate": "0/0",
            "avg_frame_rate": "0/0",
            "disposition": {"default": 0, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();
        assert_eq!(file.tracks[0].track_type, TrackType::Attachment);
    }

    #[test]
    fn test_motion_mjpeg_with_real_frames_stays_video() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "mjpeg",
            "width": 1920,
            "height": 1080,
            "r_frame_rate": "24000/1001",
            "avg_frame_rate": "24000/1001",
            "nb_frames": "138165",
            "duration": "5760.000000",
            "disposition": {"default": 1, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();
        assert_eq!(file.tracks[0].track_type, TrackType::Video);
    }

    #[test]
    fn test_hevc_with_missing_nb_frames_stays_video() {
        let stream = serde_json::json!({
            "index": 0,
            "codec_type": "video",
            "codec_name": "hevc",
            "width": 3840,
            "height": 2160,
            "r_frame_rate": "24000/1001",
            "avg_frame_rate": "24000/1001",
            "disposition": {"default": 1, "forced": 0, "attached_pic": 0, "comment": 0},
            "tags": {}
        });
        let json = make_ffprobe_json(&[stream]);
        let file = parse_ffprobe_output(&json, Path::new("/test.mkv"), 0, None).unwrap();
        assert_eq!(file.tracks[0].track_type, TrackType::Video);
    }
}
