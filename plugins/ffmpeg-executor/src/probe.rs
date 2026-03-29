//! FFmpeg capability probing: parse output from `ffmpeg -codecs`, `-formats`, `-hwaccels`.

use voom_domain::events::CodecCapabilities;

/// Parse `ffmpeg -codecs` output into decoder and encoder lists.
///
/// Each codec line (after the `-------` separator) has flags in columns 0-5:
/// `D` = decoding, `E` = encoding. The codec name follows after whitespace.
pub fn parse_codecs(output: &str) -> CodecCapabilities {
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
pub fn parse_formats(output: &str) -> Vec<String> {
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
pub fn parse_hwaccels(output: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
