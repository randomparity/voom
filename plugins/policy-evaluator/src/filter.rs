//! Track filtering: evaluates `CompiledFilter` against tracks.

use voom_domain::media::{MediaFile, Track, TrackType};
use voom_dsl::compiled::{CompiledCompareOp, CompiledFilter, TrackTarget};

use crate::condition::{resolve_field, EvalContext};

/// Returns true if the track matches the filter (no field resolution context).
///
/// `LangField` and `CodecField` filters always return `false` here because
/// they require a `MediaFile` + `EvalContext` to resolve. Use
/// [`track_matches_with_context`] when the filter may contain field references.
#[cfg(test)]
#[must_use]
pub(crate) fn track_matches(track: &Track, filter: &CompiledFilter) -> bool {
    track_matches_impl(track, filter, None)
}

/// Returns true if the track matches the filter, resolving field references
/// against the given `MediaFile` and `EvalContext`.
#[must_use]
pub fn track_matches_with_context(
    track: &Track,
    filter: &CompiledFilter,
    file: &MediaFile,
    eval_ctx: &EvalContext<'_>,
) -> bool {
    track_matches_impl(track, filter, Some((file, eval_ctx)))
}

/// Core filter matching, optionally with context for resolving field references.
fn track_matches_impl(
    track: &Track,
    filter: &CompiledFilter,
    ctx: Option<(&MediaFile, &EvalContext<'_>)>,
) -> bool {
    match filter {
        CompiledFilter::LangIn(langs) => langs.iter().any(|l| l == &track.language),
        CompiledFilter::LangCompare(op, lang) => compare_string(&track.language, *op, lang),
        CompiledFilter::LangField(op, path) => resolve_field_str(ctx, path)
            .is_some_and(|val| compare_string(&track.language, *op, &val)),
        CompiledFilter::CodecIn(codecs) => codecs.iter().any(|c| c == &track.codec),
        CompiledFilter::CodecCompare(op, codec) => compare_string(&track.codec, *op, codec),
        CompiledFilter::CodecField(op, path) => {
            resolve_field_str(ctx, path).is_some_and(|val| compare_string(&track.codec, *op, &val))
        }
        CompiledFilter::Channels(op, value) => {
            if let Some(ch) = track.channels {
                compare_f64(f64::from(ch), *op, *value)
            } else {
                false
            }
        }
        CompiledFilter::Commentary => is_commentary_track(track),
        CompiledFilter::Forced => track.is_forced,
        CompiledFilter::Default => track.is_default,
        CompiledFilter::Font => is_font_attachment(track),
        CompiledFilter::TitleContains(s) => track.title.to_lowercase().contains(&s.to_lowercase()),
        CompiledFilter::TitleMatches(compiled_re) => compiled_re.regex().is_match(&track.title),
        CompiledFilter::And(filters) => filters.iter().all(|f| track_matches_impl(track, f, ctx)),
        CompiledFilter::Or(filters) => filters.iter().any(|f| track_matches_impl(track, f, ctx)),
        CompiledFilter::Not(inner) => !track_matches_impl(track, inner, ctx),
    }
}

/// Resolve a field path to a string value using the optional context.
fn resolve_field_str(
    ctx: Option<(&MediaFile, &EvalContext<'_>)>,
    path: &[String],
) -> Option<String> {
    let (file, eval_ctx) = ctx?;
    resolve_field(file, path, eval_ctx).and_then(|v| match v {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
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
    // title is already lowercased; suffix comparisons need not be case-insensitive again.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    {
        codec_lower.contains("font")
            || codec_lower.contains("ttf")
            || codec_lower.contains("otf")
            || title_lower.ends_with(".ttf")
            || title_lower.ends_with(".otf")
            || title_lower.ends_with(".woff")
    }
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
fn compare_string(left: &str, op: CompiledCompareOp, right: &str) -> bool {
    match op {
        CompiledCompareOp::Eq => left == right,
        CompiledCompareOp::Ne => left != right,
        _ => false, // only == and != are valid for string comparisons (parser rejects others)
    }
}

/// Compare two f64 values using the given operator.
#[must_use]
pub fn compare_f64(left: f64, op: CompiledCompareOp, right: f64) -> bool {
    match op {
        CompiledCompareOp::Eq => (left - right).abs() < f64::EPSILON,
        CompiledCompareOp::Ne => (left - right).abs() >= f64::EPSILON,
        CompiledCompareOp::Lt => left < right,
        CompiledCompareOp::Le => left <= right,
        CompiledCompareOp::Gt => left > right,
        CompiledCompareOp::Ge => left >= right,
        // In is handled before reaching scalar comparison; see
        // evaluate_field_compare() which dispatches In before calling
        // compare_json/compare_f64.
        CompiledCompareOp::In => {
            debug_assert!(false, "In operator should not reach compare_f64");
            false
        }
    }
}

/// Get tracks from a file matching the given target type.
#[must_use]
pub(crate) fn tracks_for_target(file: &MediaFile, target: TrackTarget) -> Vec<&Track> {
    match target {
        TrackTarget::Video => file.video_tracks(),
        TrackTarget::Audio => file.audio_tracks(),
        TrackTarget::Subtitle => file.subtitle_tracks(),
        TrackTarget::Attachment => file.tracks_of_type(TrackType::Attachment),
        TrackTarget::Any => file.tracks.iter().collect(),
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
    fn test_image_attachment_not_font() {
        // JPEG image attachment — should NOT match font filter
        let jpeg = Track::new(0, TrackType::Attachment, "mjpeg".into());
        assert!(!track_matches(&jpeg, &CompiledFilter::Font));
        assert!(track_matches(
            &jpeg,
            &CompiledFilter::Not(Box::new(CompiledFilter::Font)),
        ));

        // PNG image attachment — should NOT match font filter
        let mut png = Track::new(1, TrackType::Attachment, "png".into());
        png.title = "poster.png".into();
        assert!(!track_matches(&png, &CompiledFilter::Font));
        assert!(track_matches(
            &png,
            &CompiledFilter::Not(Box::new(CompiledFilter::Font)),
        ));

        // Non-attachment track with image codec — font filter ignores it
        let video = Track::new(2, TrackType::Video, "mjpeg".into());
        assert!(!track_matches(&video, &CompiledFilter::Font));
    }

    #[test]
    fn test_title_contains_cover_attachment() {
        let mut track = Track::new(0, TrackType::Attachment, "mjpeg".into());
        track.title = "cover.jpg".into();
        assert!(track_matches(
            &track,
            &CompiledFilter::TitleContains("cover".into()),
        ));
        assert!(!track_matches(&track, &CompiledFilter::Font));

        // Compound filter: font OR title contains "cover"
        let compound = CompiledFilter::Or(vec![
            CompiledFilter::Font,
            CompiledFilter::TitleContains("cover".into()),
        ]);
        assert!(track_matches(&track, &compound));
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

    #[test]
    fn test_lang_field_resolves_from_plugin_metadata() {
        use std::path::PathBuf;
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"original_language": "jpn"}),
        );
        let ctx = EvalContext { capabilities: None };

        let jpn_track = audio_track("jpn", "aac", 2);
        let eng_track = audio_track("eng", "aac", 2);

        let filter = CompiledFilter::LangField(
            CompiledCompareOp::Eq,
            vec!["plugin".into(), "radarr".into(), "original_language".into()],
        );

        assert!(track_matches_with_context(&jpn_track, &filter, &file, &ctx));
        assert!(!track_matches_with_context(
            &eng_track, &filter, &file, &ctx
        ));
    }

    #[test]
    fn test_lang_field_returns_false_when_field_missing() {
        use std::path::PathBuf;
        let file = MediaFile::new(PathBuf::from("/test.mkv"));
        let ctx = EvalContext { capabilities: None };

        let track = audio_track("eng", "aac", 2);
        let filter = CompiledFilter::LangField(
            CompiledCompareOp::Eq,
            vec!["plugin".into(), "radarr".into(), "original_language".into()],
        );

        assert!(!track_matches_with_context(&track, &filter, &file, &ctx));
    }

    #[test]
    fn test_lang_field_ne() {
        use std::path::PathBuf;
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"original_language": "jpn"}),
        );
        let ctx = EvalContext { capabilities: None };

        let eng_track = audio_track("eng", "aac", 2);
        let jpn_track = audio_track("jpn", "aac", 2);

        let filter = CompiledFilter::LangField(
            CompiledCompareOp::Ne,
            vec!["plugin".into(), "radarr".into(), "original_language".into()],
        );

        assert!(track_matches_with_context(&eng_track, &filter, &file, &ctx));
        assert!(!track_matches_with_context(
            &jpn_track, &filter, &file, &ctx
        ));
    }

    #[test]
    fn test_codec_field_resolves() {
        use std::path::PathBuf;
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.plugin_metadata
            .insert("detector".into(), serde_json::json!({"codec": "aac"}));
        let ctx = EvalContext { capabilities: None };

        let aac_track = audio_track("eng", "aac", 2);
        let flac_track = audio_track("eng", "flac", 2);

        let filter = CompiledFilter::CodecField(
            CompiledCompareOp::Eq,
            vec!["plugin".into(), "detector".into(), "codec".into()],
        );

        assert!(track_matches_with_context(&aac_track, &filter, &file, &ctx));
        assert!(!track_matches_with_context(
            &flac_track,
            &filter,
            &file,
            &ctx
        ));
    }

    #[test]
    fn test_lang_field_without_context_returns_false() {
        let track = audio_track("jpn", "aac", 2);
        let filter = CompiledFilter::LangField(
            CompiledCompareOp::Eq,
            vec!["plugin".into(), "radarr".into(), "original_language".into()],
        );
        // track_matches (no context) should return false for field refs
        assert!(!track_matches(&track, &filter));
    }

    // ---- compare_f64 boundary tests (issue #236, phase 2) ----
    // Each test targets specific surviving mutants on lines 131–134. The
    // EPSILON-boundary tests use input pairs whose absolute difference is
    // exactly f64::EPSILON, the only inputs that distinguish `<` from `<=`
    // (or `>=` from `<`) on that branch. The Ne `replace - with +/replace -
    // with /` mutants need inputs where (left-right), (left+right), and
    // (left/right) all give different absolute values, which is why we use
    // `(1.0, -1.0)` and `(2.0, 2.0)`.

    #[test]
    fn compare_f64_eq_at_epsilon_boundary() {
        // |0 - EPSILON| = EPSILON. EPSILON < EPSILON is false, but the
        // `< to <=` mutant would flip this to true.
        assert!(!compare_f64(0.0, CompiledCompareOp::Eq, f64::EPSILON));
    }

    #[test]
    fn compare_f64_ne_at_epsilon_boundary() {
        // |0 - EPSILON| = EPSILON. EPSILON >= EPSILON is true, but the
        // `>= to <` mutant would flip this to false.
        assert!(compare_f64(0.0, CompiledCompareOp::Ne, f64::EPSILON));
    }

    #[test]
    fn compare_f64_ne_subtraction_distinguishes_addition() {
        // |1 - (-1)| = 2 (>= EPSILON, true). The `- to +` mutant computes
        // |1 + (-1)| = 0 (< EPSILON, false), so the result flips.
        assert!(compare_f64(1.0, CompiledCompareOp::Ne, -1.0));
    }

    #[test]
    fn compare_f64_ne_subtraction_distinguishes_division() {
        // |2 - 2| = 0 (< EPSILON, false). The `- to /` mutant computes
        // |2 / 2| = 1 (>= EPSILON, true), so the result flips.
        assert!(!compare_f64(2.0, CompiledCompareOp::Ne, 2.0));
    }

    #[test]
    fn compare_f64_lt_equal_inputs() {
        // 1 < 1 is false. Both `< to ==` (1 == 1 = true) and `< to <=`
        // (1 <= 1 = true) mutants flip the result, so this single test
        // kills two surviving mutants on line 133.
        assert!(!compare_f64(1.0, CompiledCompareOp::Lt, 1.0));
    }

    #[test]
    fn compare_f64_lt_left_less() {
        // 1 < 2 is true. The `< to >` mutant (1 > 2 = false) flips it.
        assert!(compare_f64(1.0, CompiledCompareOp::Lt, 2.0));
    }

    #[test]
    fn compare_f64_le_equal_inputs() {
        // 1 <= 1 is true. The `<= to >` mutant (1 > 1 = false) flips it.
        assert!(compare_f64(1.0, CompiledCompareOp::Le, 1.0));
    }
}
