use std::collections::HashMap;
use std::sync::LazyLock;

/// Canonical codec name mappings. Maps common aliases to canonical names.
static VIDEO_CODECS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    // H.264 / AVC
    m.insert("h264", "h264");
    m.insert("avc", "h264");
    m.insert("avc1", "h264");
    m.insert("x264", "h264");
    // H.265 / HEVC
    m.insert("h265", "hevc");
    m.insert("hevc", "hevc");
    m.insert("x265", "hevc");
    m.insert("hev1", "hevc");
    // AV1
    m.insert("av1", "av1");
    m.insert("av01", "av1");
    // VP9
    m.insert("vp9", "vp9");
    m.insert("vp09", "vp9");
    // VP8
    m.insert("vp8", "vp8");
    // MPEG-2
    m.insert("mpeg2", "mpeg2video");
    m.insert("mpeg2video", "mpeg2video");
    m.insert("m2v", "mpeg2video");
    // VC-1
    m.insert("vc1", "vc1");
    m.insert("vc-1", "vc1");
    m.insert("wmv3", "vc1");
    m
});

static AUDIO_CODECS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    // AAC
    m.insert("aac", "aac");
    m.insert("aac-lc", "aac");
    m.insert("mp4a", "aac");
    // AC-3
    m.insert("ac3", "ac3");
    m.insert("ac-3", "ac3");
    m.insert("a_ac3", "ac3");
    // E-AC-3
    m.insert("eac3", "eac3");
    m.insert("e-ac-3", "eac3");
    m.insert("a_eac3", "eac3");
    // TrueHD
    m.insert("truehd", "truehd");
    m.insert("mlp", "truehd");
    m.insert("a_truehd", "truehd");
    // DTS
    m.insert("dts", "dts");
    m.insert("a_dts", "dts");
    // DTS-HD MA
    m.insert("dts_hd", "dts_hd_ma");
    m.insert("dts-hd", "dts_hd_ma");
    m.insert("dts-hd ma", "dts_hd_ma");
    m.insert("dts_hd_ma", "dts_hd_ma");
    // FLAC
    m.insert("flac", "flac");
    m.insert("a_flac", "flac");
    // Opus
    m.insert("opus", "opus");
    m.insert("a_opus", "opus");
    // Vorbis
    m.insert("vorbis", "vorbis");
    m.insert("a_vorbis", "vorbis");
    // MP3
    m.insert("mp3", "mp3");
    m.insert("mp2", "mp2");
    // PCM
    m.insert("pcm", "pcm");
    m.insert("pcm_s16le", "pcm");
    m.insert("pcm_s24le", "pcm");
    m
});

static SUBTITLE_CODECS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("srt", "subrip");
    m.insert("subrip", "subrip");
    m.insert("s_text/utf8", "subrip");
    m.insert("ass", "ass");
    m.insert("ssa", "ass");
    m.insert("s_text/ass", "ass");
    m.insert("pgs", "hdmv_pgs_subtitle");
    m.insert("hdmv_pgs_subtitle", "hdmv_pgs_subtitle");
    m.insert("s_hdmv/pgs", "hdmv_pgs_subtitle");
    m.insert("vobsub", "dvd_subtitle");
    m.insert("dvd_subtitle", "dvd_subtitle");
    m.insert("s_vobsub", "dvd_subtitle");
    m.insert("dvb_subtitle", "dvb_subtitle");
    m.insert("webvtt", "webvtt");
    m
});

/// All known codec names for fuzzy matching / did-you-mean suggestions.
static ALL_CODEC_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut names: Vec<&str> = Vec::new();
    for &v in VIDEO_CODECS.values() {
        if !names.contains(&v) {
            names.push(v);
        }
    }
    for &v in AUDIO_CODECS.values() {
        if !names.contains(&v) {
            names.push(v);
        }
    }
    for &v in SUBTITLE_CODECS.values() {
        if !names.contains(&v) {
            names.push(v);
        }
    }
    names.sort();
    names
});

/// Normalize a codec name to its canonical form.
/// Returns `None` if the codec is not recognized.
pub fn normalize_codec(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let lower = lower.as_str();
    VIDEO_CODECS
        .get(lower)
        .or_else(|| AUDIO_CODECS.get(lower))
        .or_else(|| SUBTITLE_CODECS.get(lower))
        .copied()
}

/// Returns all known canonical codec names.
pub fn all_codec_names() -> &'static [&'static str] {
    &ALL_CODEC_NAMES
}

/// Find the closest matching codec name for "did you mean?" suggestions.
/// Returns `None` if nothing is close enough (edit distance > 3).
pub fn suggest_codec(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let mut best: Option<(&str, usize)> = None;
    for &canonical in all_codec_names() {
        let dist = edit_distance(&lower, canonical);
        if dist <= 3 && (best.is_none() || dist < best.unwrap().1) {
            best = Some((canonical, dist));
        }
    }
    // Also check all aliases
    for table in [&*VIDEO_CODECS, &*AUDIO_CODECS, &*SUBTITLE_CODECS] {
        for (&alias, &canonical) in table {
            let dist = edit_distance(&lower, alias);
            if dist <= 3 && (best.is_none() || dist < best.unwrap().1) {
                best = Some((canonical, dist));
            }
        }
    }
    best.map(|(name, _)| name)
}

/// Simple Levenshtein edit distance.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let m = a_bytes.len();
    let n = b_bytes.len();

    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_video_codecs() {
        assert_eq!(normalize_codec("h264"), Some("h264"));
        assert_eq!(normalize_codec("avc"), Some("h264"));
        assert_eq!(normalize_codec("H265"), Some("hevc"));
        assert_eq!(normalize_codec("hevc"), Some("hevc"));
        assert_eq!(normalize_codec("av1"), Some("av1"));
    }

    #[test]
    fn test_normalize_audio_codecs() {
        assert_eq!(normalize_codec("aac"), Some("aac"));
        assert_eq!(normalize_codec("ac3"), Some("ac3"));
        assert_eq!(normalize_codec("truehd"), Some("truehd"));
        assert_eq!(normalize_codec("dts"), Some("dts"));
        assert_eq!(normalize_codec("flac"), Some("flac"));
        assert_eq!(normalize_codec("opus"), Some("opus"));
    }

    #[test]
    fn test_normalize_subtitle_codecs() {
        assert_eq!(normalize_codec("srt"), Some("subrip"));
        assert_eq!(normalize_codec("ass"), Some("ass"));
        assert_eq!(normalize_codec("pgs"), Some("hdmv_pgs_subtitle"));
    }

    #[test]
    fn test_unknown_codec() {
        assert_eq!(normalize_codec("nonexistent"), None);
    }

    #[test]
    fn test_suggest_codec() {
        // Typo: hev instead of hevc
        assert_eq!(suggest_codec("hev"), Some("hevc"));
        // Typo: flacc instead of flac
        assert_eq!(suggest_codec("flacc"), Some("flac"));
        // Too far off
        assert_eq!(suggest_codec("zzzzzzzzz"), None);
    }
}
