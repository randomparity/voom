//! Track filtering: evaluates `CompiledFilter` against tracks.

use voom_domain::media::{Track, TrackType};
use voom_dsl::compiler::CompiledCompareOp;
use voom_dsl::compiler::CompiledFilter;

/// Returns true if the track matches the filter.
#[must_use]
pub fn track_matches(track: &Track, filter: &CompiledFilter) -> bool {
    match filter {
        CompiledFilter::LangIn(langs) => langs.iter().any(|l| l == &track.language),
        CompiledFilter::LangCompare(op, lang) => compare_string(&track.language, op, lang),
        CompiledFilter::CodecIn(codecs) => codecs.iter().any(|c| c == &track.codec),
        CompiledFilter::CodecCompare(op, codec) => compare_string(&track.codec, op, codec),
        CompiledFilter::Channels(op, value) => {
            if let Some(ch) = track.channels {
                compare_f64(ch as f64, op, *value)
            } else {
                false
            }
        }
        CompiledFilter::Commentary => is_commentary_track(track),
        CompiledFilter::Forced => track.is_forced,
        CompiledFilter::Default => track.is_default,
        CompiledFilter::Font => is_font_attachment(track),
        CompiledFilter::TitleContains(s) => track.title.to_lowercase().contains(&s.to_lowercase()),
        CompiledFilter::TitleMatches(pattern) => regex::Regex::new(pattern)
            .map(|re| re.is_match(&track.title))
            .unwrap_or(false),
        CompiledFilter::And(filters) => filters.iter().all(|f| track_matches(track, f)),
        CompiledFilter::Or(filters) => filters.iter().any(|f| track_matches(track, f)),
        CompiledFilter::Not(inner) => !track_matches(track, inner),
    }
}

/// Check if a track is a commentary track based on its `TrackType`.
fn is_commentary_track(track: &Track) -> bool {
    matches!(
        track.track_type,
        TrackType::AudioCommentary | TrackType::SubtitleCommentary
    )
}

/// Check if a track is a font attachment.
fn is_font_attachment(track: &Track) -> bool {
    if track.track_type != TrackType::Attachment {
        return false;
    }
    let codec_lower = track.codec.to_lowercase();
    let title_lower = track.title.to_lowercase();
    codec_lower.contains("font")
        || codec_lower.contains("ttf")
        || codec_lower.contains("otf")
        || title_lower.ends_with(".ttf")
        || title_lower.ends_with(".otf")
        || title_lower.ends_with(".woff")
}

/// Check if a track is a commentary track based on title patterns.
#[must_use]
pub fn is_commentary_by_pattern(track: &Track, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return is_commentary_track(track);
    }
    let title_lower = track.title.to_lowercase();
    patterns
        .iter()
        .any(|p| title_lower.contains(&p.to_lowercase()))
        || is_commentary_track(track)
}

/// Compare two strings using the given operator (supports Eq and Ne).
fn compare_string(left: &str, op: &CompiledCompareOp, right: &str) -> bool {
    match op {
        CompiledCompareOp::Eq => left == right,
        CompiledCompareOp::Ne => left != right,
        _ => false, // only == and != are valid for string comparisons (parser rejects others)
    }
}

/// Compare two f64 values using the given operator.
#[must_use]
pub fn compare_f64(left: f64, op: &CompiledCompareOp, right: f64) -> bool {
    match op {
        CompiledCompareOp::Eq => (left - right).abs() < f64::EPSILON,
        CompiledCompareOp::Ne => (left - right).abs() >= f64::EPSILON,
        CompiledCompareOp::Lt => left < right,
        CompiledCompareOp::Le => left <= right,
        CompiledCompareOp::Gt => left > right,
        CompiledCompareOp::Ge => left >= right,
        CompiledCompareOp::In => false, // In is not valid for scalar comparison
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::media::Track;

    fn audio_track(lang: &str, codec: &str, channels: u32) -> Track {
        let mut t = Track::new(0, TrackType::AudioMain, codec.into());
        t.language = lang.into();
        t.channels = Some(channels);
        t
    }

    #[test]
    fn test_lang_in_filter() {
        let track = audio_track("eng", "aac", 2);
        assert!(track_matches(
            &track,
            &CompiledFilter::LangIn(vec!["eng".into(), "jpn".into()])
        ));
        assert!(!track_matches(
            &track,
            &CompiledFilter::LangIn(vec!["jpn".into()])
        ));
    }

    #[test]
    fn test_codec_in_filter() {
        let track = audio_track("eng", "aac", 2);
        assert!(track_matches(
            &track,
            &CompiledFilter::CodecIn(vec!["aac".into()])
        ));
        assert!(!track_matches(
            &track,
            &CompiledFilter::CodecIn(vec!["flac".into()])
        ));
    }

    #[test]
    fn test_channels_filter() {
        let track = audio_track("eng", "aac", 6);
        assert!(track_matches(
            &track,
            &CompiledFilter::Channels(CompiledCompareOp::Ge, 6.0)
        ));
        assert!(!track_matches(
            &track,
            &CompiledFilter::Channels(CompiledCompareOp::Gt, 6.0)
        ));
    }

    #[test]
    fn test_commentary_filter() {
        let mut track = Track::new(0, TrackType::AudioCommentary, "aac".into());
        track.language = "eng".into();
        assert!(track_matches(&track, &CompiledFilter::Commentary));

        let main = audio_track("eng", "aac", 2);
        assert!(!track_matches(&main, &CompiledFilter::Commentary));
    }

    #[test]
    fn test_forced_filter() {
        let mut track = audio_track("eng", "aac", 2);
        assert!(!track_matches(&track, &CompiledFilter::Forced));
        track.is_forced = true;
        assert!(track_matches(&track, &CompiledFilter::Forced));
    }

    #[test]
    fn test_default_filter() {
        let mut track = audio_track("eng", "aac", 2);
        assert!(!track_matches(&track, &CompiledFilter::Default));
        track.is_default = true;
        assert!(track_matches(&track, &CompiledFilter::Default));
    }

    #[test]
    fn test_font_filter() {
        let mut track = Track::new(0, TrackType::Attachment, "font/ttf".into());
        track.title = "Arial.ttf".into();
        assert!(track_matches(&track, &CompiledFilter::Font));

        let not_font = Track::new(1, TrackType::Attachment, "image/jpeg".into());
        assert!(!track_matches(&not_font, &CompiledFilter::Font));
    }

    #[test]
    fn test_title_contains_filter() {
        let mut track = audio_track("eng", "aac", 2);
        track.title = "Director's Commentary".into();
        assert!(track_matches(
            &track,
            &CompiledFilter::TitleContains("commentary".into())
        ));
        assert!(!track_matches(
            &track,
            &CompiledFilter::TitleContains("stereo".into())
        ));
    }

    #[test]
    fn test_and_or_not_filters() {
        let track = audio_track("eng", "aac", 2);
        // eng AND aac
        let and_filter = CompiledFilter::And(vec![
            CompiledFilter::LangIn(vec!["eng".into()]),
            CompiledFilter::CodecIn(vec!["aac".into()]),
        ]);
        assert!(track_matches(&track, &and_filter));

        // jpn OR eng
        let or_filter = CompiledFilter::Or(vec![
            CompiledFilter::LangIn(vec!["jpn".into()]),
            CompiledFilter::LangIn(vec!["eng".into()]),
        ]);
        assert!(track_matches(&track, &or_filter));

        // NOT jpn
        let not_filter = CompiledFilter::Not(Box::new(CompiledFilter::LangIn(vec!["jpn".into()])));
        assert!(track_matches(&track, &not_filter));
    }

    #[test]
    fn test_commentary_by_pattern() {
        let mut track = audio_track("eng", "aac", 2);
        track.title = "Director's Commentary Track".into();
        let patterns = vec!["commentary".into(), "director".into()];
        assert!(is_commentary_by_pattern(&track, &patterns));

        track.title = "Main Audio".into();
        assert!(!is_commentary_by_pattern(&track, &patterns));
    }

    #[test]
    fn test_lang_compare_ne() {
        let jpn_track = audio_track("jpn", "aac", 2);
        let eng_track = audio_track("eng", "aac", 2);

        let filter = CompiledFilter::LangCompare(CompiledCompareOp::Ne, "jpn".into());
        // "lang != jpn" should NOT match a jpn track
        assert!(!track_matches(&jpn_track, &filter));
        // "lang != jpn" SHOULD match an eng track
        assert!(track_matches(&eng_track, &filter));
    }

    #[test]
    fn test_codec_compare_ne() {
        let aac_track = audio_track("eng", "aac", 2);
        let flac_track = audio_track("eng", "flac", 2);

        let filter = CompiledFilter::CodecCompare(CompiledCompareOp::Ne, "aac".into());
        assert!(!track_matches(&aac_track, &filter));
        assert!(track_matches(&flac_track, &filter));
    }

    #[test]
    fn test_lang_compare_eq() {
        let track = audio_track("jpn", "aac", 2);
        let filter = CompiledFilter::LangCompare(CompiledCompareOp::Eq, "jpn".into());
        assert!(track_matches(&track, &filter));
        let filter_other = CompiledFilter::LangCompare(CompiledCompareOp::Eq, "eng".into());
        assert!(!track_matches(&track, &filter_other));
    }
}
