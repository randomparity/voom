//! Codec/container compatibility matrix.
//!
//! Used by the `ContainerIncompatible` safeguard to detect when a planned
//! container conversion would produce an output whose surviving tracks use
//! codecs the target container cannot hold.
//!
//! The matrix is deliberately conservative: only containers with a
//! well-understood codec whitelist are modeled. Unmodeled containers return
//! `None` so the safeguard skips the check rather than producing false
//! positives.

use voom_domain::media::Container;

/// Check whether a container can hold a track with the given codec.
///
/// Returns:
/// - `Some(true)` — container is modeled and supports the codec.
/// - `Some(false)` — container is modeled and does NOT support the codec.
/// - `None` — container is not modeled; caller should skip the check.
///
/// Codec strings are matched case-insensitively against ffprobe
/// `codec_name` values (e.g. `h264`, `hevc`, `opus`, `aac`, `subrip`).
#[must_use]
pub fn codec_supported(container: Container, codec: &str) -> Option<bool> {
    let codec = codec.trim().to_ascii_lowercase();
    if codec.is_empty() {
        // No codec info — can't judge. Skip.
        return None;
    }
    // Explicit arm for known unmodeled containers plus a wildcard for
    // future non_exhaustive variants; the duplication is deliberate.
    #[allow(clippy::match_same_arms)]
    match container {
        // MKV is effectively a universal container: it can carry virtually
        // any codec we encounter in practice. Treat every non-empty codec
        // as supported.
        Container::Mkv => Some(true),
        Container::Mp4 => Some(mp4_supports(&codec)),
        Container::Webm => Some(webm_supports(&codec)),
        Container::Avi => Some(avi_supports(&codec)),
        // Containers we do not model: skip the check.
        Container::Mov | Container::Ts | Container::Flv | Container::Wmv | Container::Other => None,
        // `Container` is #[non_exhaustive]; any future variant defaults to "skip".
        _ => None,
    }
}

fn mp4_supports(codec: &str) -> bool {
    matches!(
        codec,
        // Video
        "h264"
            | "avc1"
            | "hevc"
            | "h265"
            | "hev1"
            | "hvc1"
            | "av1"
            | "mpeg4"
            | "mpeg2video"
            | "vp9"
            // Audio
            | "aac"
            | "mp3"
            | "ac3"
            | "eac3"
            | "opus"
            | "alac"
            | "flac"
            // Subtitles
            | "mov_text"
            | "tx3g"
    )
}

fn webm_supports(codec: &str) -> bool {
    matches!(
        codec,
        // Video
        "vp8" | "vp9" | "av1"
        // Audio
        | "opus" | "vorbis"
        // Subtitles
        | "webvtt"
    )
}

fn avi_supports(codec: &str) -> bool {
    matches!(
        codec,
        // Video
        "mpeg4"
            | "mpeg2video"
            | "mpeg1video"
            | "mjpeg"
            | "h264"
            | "avc1"
            // Audio
            | "mp3"
            | "ac3"
            | "aac"
            | "pcm_s16le"
            | "pcm_s24le"
            | "pcm_u8"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkv_accepts_everything() {
        for codec in ["h264", "hevc", "opus", "ac3", "flac", "subrip", "vp9"] {
            assert_eq!(codec_supported(Container::Mkv, codec), Some(true));
        }
    }

    #[test]
    fn mp4_accepts_common_codecs() {
        for codec in ["h264", "hevc", "av1", "aac", "ac3", "opus", "mov_text"] {
            assert_eq!(codec_supported(Container::Mp4, codec), Some(true));
        }
    }

    #[test]
    fn mp4_rejects_incompatible_codecs() {
        for codec in [
            "vp8",
            "vorbis",
            "subrip",
            "ass",
            "ssa",
            "dts",
            "truehd",
            "pcm_s16le",
            "hdmv_pgs_subtitle",
        ] {
            assert_eq!(
                codec_supported(Container::Mp4, codec),
                Some(false),
                "expected mp4 to reject {codec}"
            );
        }
    }

    #[test]
    fn webm_accepts_only_open_codecs() {
        for codec in ["vp8", "vp9", "av1", "opus", "vorbis", "webvtt"] {
            assert_eq!(codec_supported(Container::Webm, codec), Some(true));
        }
    }

    #[test]
    fn webm_rejects_proprietary_codecs() {
        for codec in ["h264", "hevc", "aac", "ac3", "subrip", "mov_text"] {
            assert_eq!(
                codec_supported(Container::Webm, codec),
                Some(false),
                "expected webm to reject {codec}"
            );
        }
    }

    #[test]
    fn avi_accepts_legacy_codecs() {
        for codec in ["mpeg4", "mpeg2video", "mjpeg", "mp3", "ac3", "pcm_s16le"] {
            assert_eq!(codec_supported(Container::Avi, codec), Some(true));
        }
    }

    #[test]
    fn avi_rejects_modern_codecs() {
        for codec in ["hevc", "av1", "vp9", "opus", "flac", "vorbis", "subrip"] {
            assert_eq!(
                codec_supported(Container::Avi, codec),
                Some(false),
                "expected avi to reject {codec}"
            );
        }
    }

    #[test]
    fn unmodeled_containers_return_none() {
        for container in [
            Container::Mov,
            Container::Ts,
            Container::Flv,
            Container::Wmv,
            Container::Other,
        ] {
            assert_eq!(codec_supported(container, "h264"), None);
        }
    }

    #[test]
    fn empty_codec_returns_none() {
        assert_eq!(codec_supported(Container::Mp4, ""), None);
        assert_eq!(codec_supported(Container::Mp4, "   "), None);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(codec_supported(Container::Mp4, "H264"), Some(true));
        assert_eq!(codec_supported(Container::Webm, "OPUS"), Some(true));
        assert_eq!(codec_supported(Container::Avi, "Hevc"), Some(false));
    }
}
