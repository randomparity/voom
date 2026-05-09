//! Condition evaluation: evaluates `CompiledCondition` against a `MediaFile`.

use std::collections::HashSet;

use voom_domain::media::MediaFile;
use voom_dsl::compiled::{CompiledCompareOp, CompiledCondition};

use crate::field::{resolve_field, EvalContext};
use crate::filter::{compare_f64, track_matches_with_context, tracks_for_target};

/// Evaluate a condition against a media file.
#[must_use]
pub fn evaluate_condition(
    cond: &CompiledCondition,
    file: &MediaFile,
    ctx: &EvalContext<'_>,
) -> bool {
    match cond {
        CompiledCondition::Exists { target, filter } => {
            let tracks = tracks_for_target(file, *target);
            match filter {
                Some(f) => tracks
                    .iter()
                    .any(|t| track_matches_with_context(t, f, file, ctx)),
                None => !tracks.is_empty(),
            }
        }
        CompiledCondition::Count {
            target,
            filter,
            op,
            value,
        } => {
            let tracks = tracks_for_target(file, *target);
            let count = match filter {
                Some(f) => tracks
                    .iter()
                    .filter(|t| track_matches_with_context(t, f, file, ctx))
                    .count(),
                None => tracks.len(),
            };
            // Track counts in practice fit well within f64 mantissa precision.
            #[allow(clippy::cast_precision_loss)]
            let count_f = count as f64;
            compare_f64(count_f, *op, *value)
        }
        CompiledCondition::FieldCompare { path, op, value } => {
            evaluate_field_compare(file, path, *op, value, ctx)
        }
        CompiledCondition::FieldExists { path } => resolve_field(file, path, ctx).is_some(),
        CompiledCondition::AudioIsMultiLanguage => audio_lang_count(file) > 1,
        CompiledCondition::IsDubbed => {
            audio_lang_count(file) > 1 && !file.subtitle_tracks().is_empty()
        }
        CompiledCondition::IsOriginal => audio_lang_count(file) <= 1,
        CompiledCondition::And(conditions) => {
            conditions.iter().all(|c| evaluate_condition(c, file, ctx))
        }
        CompiledCondition::Or(conditions) => {
            conditions.iter().any(|c| evaluate_condition(c, file, ctx))
        }
        CompiledCondition::Not(inner) => !evaluate_condition(inner, file, ctx),
    }
}

/// Count distinct audio languages (excluding "und").
fn audio_lang_count(file: &MediaFile) -> usize {
    let langs: HashSet<&str> = file
        .audio_tracks()
        .iter()
        .map(|t| t.language.as_str())
        .filter(|l| *l != "und")
        .collect();
    langs.len()
}

/// Evaluate a field comparison condition.
fn evaluate_field_compare(
    file: &MediaFile,
    path: &[String],
    op: CompiledCompareOp,
    value: &serde_json::Value,
    ctx: &EvalContext<'_>,
) -> bool {
    let Some(resolved) = resolve_field(file, path, ctx) else {
        return false;
    };

    // Handle "In" operator: check if field value is in a list
    if op == CompiledCompareOp::In {
        if let serde_json::Value::Array(list) = value {
            return list.iter().any(|v| json_values_equal(&resolved, v));
        }
        return false;
    }

    compare_json(&resolved, op, value)
}

/// Compare two JSON values.
fn compare_json(
    left: &serde_json::Value,
    op: CompiledCompareOp,
    right: &serde_json::Value,
) -> bool {
    match (left, right) {
        (serde_json::Value::Number(l), serde_json::Value::Number(r)) => {
            let (Some(lf), Some(rf)) = (l.as_f64(), r.as_f64()) else {
                return false;
            };
            compare_f64(lf, op, rf)
        }
        (serde_json::Value::String(l), serde_json::Value::String(r)) => match op {
            CompiledCompareOp::Eq => l == r,
            CompiledCompareOp::Ne => l != r,
            CompiledCompareOp::Lt => l < r,
            CompiledCompareOp::Le => l <= r,
            CompiledCompareOp::Gt => l > r,
            CompiledCompareOp::Ge => l >= r,
            // In is handled before reaching compare_json; see
            // evaluate_field_compare() which dispatches In early.
            CompiledCompareOp::In => {
                debug_assert!(false, "In operator should not reach compare_json");
                false
            }
        },
        (serde_json::Value::Bool(l), serde_json::Value::Bool(r)) => match op {
            CompiledCompareOp::Eq => l == r,
            CompiledCompareOp::Ne => l != r,
            // Ordering and In are not meaningful for booleans
            _ => false,
        },
        _ => false,
    }
}

fn json_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        // Numbers need special handling: compare as f64 so 1 == 1.0
        (serde_json::Value::Number(l), serde_json::Value::Number(r)) => l.as_f64() == r.as_f64(),
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::*;
    use std::path::PathBuf;
    use voom_domain::media::{MediaFile, Track, TrackType};
    use voom_domain::plan::PhaseOutput;
    use voom_domain::test_support::test_media_file;
    use voom_dsl::compiled::TrackTarget;

    fn test_file() -> MediaFile {
        test_media_file()
    }

    fn no_ctx() -> EvalContext<'static> {
        EvalContext::empty()
    }

    #[test]
    fn test_exists_condition() {
        let file = test_file();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: None,
            },
            &file,
            &ctx,
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Attachment,
                filter: None,
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_exists_with_filter() {
        use voom_dsl::compiled::CompiledFilter;
        let file = test_file();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(CompiledFilter::LangIn(vec!["jpn".into()])),
            },
            &file,
            &ctx,
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(CompiledFilter::LangIn(vec!["fre".into()])),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_count_condition() {
        let file = test_file();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::Count {
                target: TrackTarget::Audio,
                filter: None,
                op: CompiledCompareOp::Eq,
                value: 2.0,
            },
            &file,
            &ctx,
        ));
        assert!(evaluate_condition(
            &CompiledCondition::Count {
                target: TrackTarget::Audio,
                filter: None,
                op: CompiledCompareOp::Gt,
                value: 1.0,
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_field_compare_video_codec() {
        let file = test_file();
        let ctx = no_ctx();
        // video.codec == "hevc"
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("hevc".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_field_compare_in_list() {
        let file = test_file();
        let ctx = no_ctx();
        // video.codec in [hevc, h264]
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::In,
                value: serde_json::json!(["hevc", "h264"]),
            },
            &file,
            &ctx,
        ));
        // video.codec in [h264, vp9]
        assert!(!evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::In,
                value: serde_json::json!(["h264", "vp9"]),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_field_exists() {
        let mut file = test_file();
        let ctx = no_ctx();
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"original_language": "eng"}),
        );
        assert!(evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["plugin".into(), "radarr".into(), "original_language".into()],
            },
            &file,
            &ctx,
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["plugin".into(), "sonarr".into(), "title".into()],
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_audio_is_multi_language() {
        let file = test_file(); // eng + jpn
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::AudioIsMultiLanguage,
            &file,
            &ctx,
        ));

        let mut mono = MediaFile::new(PathBuf::from("/test.mkv"));
        mono.tracks = vec![{
            let mut t = Track::new(0, TrackType::AudioMain, "aac".into());
            t.language = "eng".into();
            t
        }];
        assert!(!evaluate_condition(
            &CompiledCondition::AudioIsMultiLanguage,
            &mono,
            &ctx,
        ));
    }

    #[test]
    fn test_and_or_not_conditions() {
        let file = test_file();
        let ctx = no_ctx();
        // Has eng audio AND jpn audio
        let cond = CompiledCondition::And(vec![
            CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(voom_dsl::compiled::CompiledFilter::LangIn(vec![
                    "eng".into()
                ])),
            },
            CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(voom_dsl::compiled::CompiledFilter::LangIn(vec![
                    "jpn".into()
                ])),
            },
        ]);
        assert!(evaluate_condition(&cond, &file, &ctx));

        // NOT has french audio
        let not_cond = CompiledCondition::Not(Box::new(CompiledCondition::Exists {
            target: TrackTarget::Audio,
            filter: Some(voom_dsl::compiled::CompiledFilter::LangIn(vec![
                "fre".into()
            ])),
        }));
        assert!(evaluate_condition(&not_cond, &file, &ctx));
    }

    #[test]
    fn test_resolve_plugin_field() {
        let mut file = test_file();
        let ctx = no_ctx();
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"title": "Test Movie", "original_language": "eng"}),
        );

        let val = resolve_value_or_field(
            &voom_dsl::compiled::CompiledValueOrField::Field(vec![
                "plugin".into(),
                "radarr".into(),
                "title".into(),
            ]),
            &file,
            &ctx,
        );
        assert_eq!(val, Some("Test Movie".into()));
    }

    #[test]
    fn test_resolve_value() {
        let file = test_file();
        let ctx = no_ctx();
        let val = resolve_value_or_field(
            &voom_dsl::compiled::CompiledValueOrField::Value(serde_json::Value::String(
                "literal".into(),
            )),
            &file,
            &ctx,
        );
        assert_eq!(val, Some("literal".into()));
    }

    #[test]
    fn test_system_hwaccel_with_capabilities() {
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        let file = test_file();
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into(), "vaapi".into()],
        ));
        let ctx = EvalContext::with_capabilities(Some(&map));

        // system.hwaccel == "nvenc"
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["system".into(), "hwaccel".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("nvenc".into()),
            },
            &file,
            &ctx,
        ));

        // system.has_hwaccel == true
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["system".into(), "has_hwaccel".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(true),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_system_hwaccel_none_without_capabilities() {
        let file = test_file();
        let ctx = no_ctx();

        // system.hwaccel resolves to None when no capabilities
        assert!(!evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["system".into(), "hwaccel".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("nvenc".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn test_system_hwaccel_no_hwaccel() {
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        let file = test_file();
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg",
            CodecCapabilities::empty(),
            vec![],
            vec![], // no hwaccels
        ));
        let ctx = EvalContext::with_capabilities(Some(&map));

        // system.hwaccel == "none"
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["system".into(), "hwaccel".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("none".into()),
            },
            &file,
            &ctx,
        ));

        // system.has_hwaccel == false
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["system".into(), "has_hwaccel".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(false),
            },
            &file,
            &ctx,
        ));
    }

    // ---- IsDubbed / IsOriginal predicate tests (issue #236, cluster D) ----
    // Targets the six surviving mutants on condition.rs:59 and :61. The four
    // IsDubbed cases pick boundary corners so each operator flip changes the
    // boolean output.

    fn file_with_audio_langs(langs: &[&str]) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.tracks = langs
            .iter()
            .enumerate()
            .map(|(i, lang)| {
                let mut t = Track::new(
                    i as u32,
                    if i == 0 {
                        TrackType::AudioMain
                    } else {
                        TrackType::AudioAlternate
                    },
                    "aac".into(),
                );
                t.language = (*lang).into();
                t
            })
            .collect();
        file
    }

    fn add_subtitle(file: &mut MediaFile, lang: &str) {
        let next_idx = file.tracks.len() as u32;
        let mut sub = Track::new(next_idx, TrackType::SubtitleMain, "srt".into());
        sub.language = lang.into();
        file.tracks.push(sub);
    }

    #[test]
    fn is_dubbed_true_when_multi_lang_with_subtitles() {
        // count=2, !empty=true → original true. Kills `> -> <` (2<1 false).
        let mut file = file_with_audio_langs(&["eng", "jpn"]);
        add_subtitle(&mut file, "eng");
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::IsDubbed,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_dubbed_false_when_single_lang_with_subtitles() {
        // count=1, !empty=true → original false.
        // Kills `> -> ==` (1==1 true), `> -> >=` (1>=1 true), and
        // `&& -> ||` first form (false || true = true).
        let mut file = file_with_audio_langs(&["eng"]);
        add_subtitle(&mut file, "eng");
        let ctx = no_ctx();
        assert!(!evaluate_condition(
            &CompiledCondition::IsDubbed,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_dubbed_false_when_multi_lang_no_subtitles() {
        // count=2, !empty=false → original false.
        // Kills `delete !` (count>1 true && empty=true → mutant true) and
        // reinforces the `&& -> ||` second form (true || false = true).
        let file = file_with_audio_langs(&["eng", "jpn"]);
        let ctx = no_ctx();
        assert!(!evaluate_condition(
            &CompiledCondition::IsDubbed,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_dubbed_false_when_no_audio_no_subtitles() {
        // count=0, no subs → original false. Sanity baseline.
        let file = MediaFile::new(PathBuf::from("/test.mkv"));
        let ctx = no_ctx();
        assert!(!evaluate_condition(
            &CompiledCondition::IsDubbed,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_original_true_with_one_audio_lang() {
        // count=1 → original `1 <= 1` true. Kills `<= -> >` (1>1 false).
        let file = file_with_audio_langs(&["eng"]);
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::IsOriginal,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_original_true_with_zero_audio_langs() {
        // count=0 → original true. Reinforces the boundary direction.
        let file = MediaFile::new(PathBuf::from("/test.mkv"));
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::IsOriginal,
            &file,
            &ctx
        ));
    }

    #[test]
    fn is_original_false_with_multi_audio_langs() {
        // count=2 → original false. Confirms `<= -> >` direction (2>1 true).
        let file = file_with_audio_langs(&["eng", "jpn"]);
        let ctx = no_ctx();
        assert!(!evaluate_condition(
            &CompiledCondition::IsOriginal,
            &file,
            &ctx
        ));
    }

    // ---- resolve_track_field / resolve_system_field aliases (issue #236, cluster E) ----
    // Each test targets a single match arm. Deleting the arm makes resolve_*
    // return None, which flips FieldExists from true to false (or makes
    // FieldCompare unable to find a value, returning false).

    fn file_with_seeded_audio() -> MediaFile {
        // First audio track has language=eng, channels=6, title="Director's Cut".
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.tracks = vec![{
            let mut t = Track::new(0, TrackType::AudioMain, "aac".into());
            t.language = "eng".into();
            t.title = "Director's Cut".into();
            t.channels = Some(6);
            t
        }];
        file
    }

    #[test]
    fn resolve_track_field_language_alias_long_form() {
        // Kills `delete match arm "language" | "lang"` via the long form.
        let file = file_with_seeded_audio();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["audio".into(), "language".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("eng".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_language_alias_short_form() {
        // Reinforces the same arm via the `lang` literal.
        let file = file_with_seeded_audio();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["audio".into(), "lang".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("eng".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_title() {
        // Kills `delete match arm "title"`.
        let file = file_with_seeded_audio();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["audio".into(), "title".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("Director's Cut".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_channels() {
        // Kills `delete match arm "channels"`.
        let file = file_with_seeded_audio();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["audio".into(), "channels".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(6),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_system_field_hwaccels_array() {
        // Kills `delete match arm "hwaccels"` in resolve_system_field.
        // FieldExists is sufficient: deleting the arm makes the function
        // fall through to `_ => None`, flipping the assertion.
        use voom_domain::capability_map::CapabilityMap;
        use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

        let file = file_with_seeded_audio();
        let mut map = CapabilityMap::new();
        map.register(ExecutorCapabilitiesEvent::new(
            "ffmpeg",
            CodecCapabilities::empty(),
            vec![],
            vec!["cuda".into(), "vaapi".into()],
        ));
        let ctx = EvalContext::with_capabilities(Some(&map));

        assert!(evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["system".into(), "hwaccels".into()],
            },
            &file,
            &ctx,
        ));
    }

    // ---- resolve_track_field video-track arms (issue #240) ----
    // Each test targets a single match arm. Seeding the field with a
    // known non-default value and asserting FieldCompare(Eq) succeeds
    // means deleting the arm — which makes resolve_track_field return
    // None — flips the assertion to false and kills the mutant.

    fn file_with_seeded_video() -> MediaFile {
        // First video track has width=1920, height=1080, frame_rate=24.0,
        // is_default/is_forced/is_hdr/is_vfr=true, hdr_format="HDR10".
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.tracks = vec![{
            let mut t = Track::new(0, TrackType::Video, "hevc".into());
            t.width = Some(1920);
            t.height = Some(1080);
            t.frame_rate = Some(24.0);
            t.is_default = true;
            t.is_forced = true;
            t.is_hdr = true;
            t.is_vfr = true;
            t.hdr_format = Some("HDR10".into());
            t
        }];
        file
    }

    #[test]
    fn resolve_track_field_width() {
        // Kills `delete match arm "width"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "width".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(1920),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_height() {
        // Kills `delete match arm "height"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "height".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(1080),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_frame_rate() {
        // Kills `delete match arm "frame_rate"`. Uses 24.0 (exactly
        // representable) to avoid f64 equality flakes.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "frame_rate".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(24.0),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_is_default() {
        // Kills `delete match arm "is_default"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "is_default".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(true),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_is_forced() {
        // Kills `delete match arm "is_forced"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "is_forced".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(true),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_is_hdr() {
        // Kills `delete match arm "is_hdr"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "is_hdr".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(true),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_is_vfr() {
        // Kills `delete match arm "is_vfr"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "is_vfr".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(true),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_track_field_hdr_format() {
        // Kills `delete match arm "hdr_format"`.
        let file = file_with_seeded_video();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "hdr_format".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("HDR10".into()),
            },
            &file,
            &ctx,
        ));
    }

    // ---- resolve_file_field arms (issue #236, phase 2) ----
    // Each test targets a single match arm in resolve_file_field. Seeding the
    // field with a known non-default value and asserting FieldCompare(Eq)
    // succeeds means deleting the arm — which makes resolve_file_field fall
    // through to the tag-lookup `_` branch (which finds nothing because the
    // helper leaves `tags` empty) — flips the assertion to false. The same
    // assertions also kill the function-level `replace -> None` and
    // `replace -> Some(Default::default())` (Value::Null) mutants because
    // each compares against a specific, non-null expected value.

    fn file_with_seeded_file_fields() -> MediaFile {
        use voom_domain::media::Container;
        let mut file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
        file.container = Container::Mkv;
        file.size = 12_345_678;
        file.duration = 90.5;
        // The cluster's tests rely on `tags` being empty so that the `_` arm of
        // resolve_file_field cannot accidentally satisfy a built-in field name.
        // Assert the precondition so a future change to MediaFile::new defaults
        // surfaces here instead of silently weakening the mutant kills.
        assert!(
            file.tags.is_empty(),
            "fixture precondition: tags must be empty"
        );
        file
    }

    #[test]
    fn resolve_file_field_container() {
        // Kills `delete match arm "container"`.
        let file = file_with_seeded_file_fields();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["file".into(), "container".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("mkv".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_file_field_size() {
        // Kills `delete match arm "size"`.
        let file = file_with_seeded_file_fields();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["file".into(), "size".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(12_345_678u64),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_file_field_duration() {
        // Kills `delete match arm "duration"`. 90.5 is exactly representable
        // in f64 to avoid equality flakes.
        let file = file_with_seeded_file_fields();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["file".into(), "duration".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::json!(90.5),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_file_field_path() {
        // Kills `delete match arm "path"`.
        let file = file_with_seeded_file_fields();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["file".into(), "path".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("/movies/test.mkv".into()),
            },
            &file,
            &ctx,
        ));
    }

    #[test]
    fn resolve_file_field_filename() {
        // Kills `delete match arm "filename"`.
        let file = file_with_seeded_file_fields();
        let ctx = no_ctx();
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["file".into(), "filename".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("test.mkv".into()),
            },
            &file,
            &ctx,
        ));
    }

    // ---- compare_json comparison-operator tests (issue #236, phase 2) ----
    // Each test calls compare_json directly via super::* so we don't have to
    // route String/Bool fixtures through FieldCompare. The String tests use
    // single-character strings to make the lexicographic ordering obvious;
    // the Bool test exercises the only valid bool comparison op (Ne) since
    // ordering ops fall through to `_ => false` regardless.

    #[test]
    fn compare_json_returns_false_for_type_mismatch() {
        // Kills `replace compare_json -> bool with true`: the type-mismatch
        // fallback must return false, not true.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::json!(1);
        assert!(!compare_json(&left, CompiledCompareOp::Eq, &right));
    }

    #[test]
    fn compare_json_string_ne_different() {
        // Kills String Ne `!= -> ==`: distinct strings should be Ne-true.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("b".into());
        assert!(compare_json(&left, CompiledCompareOp::Ne, &right));
    }

    #[test]
    fn compare_json_string_lt_equal_inputs() {
        // Kills String Lt `< -> ==` and `< -> <=`: equal strings are not Lt.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("a".into());
        assert!(!compare_json(&left, CompiledCompareOp::Lt, &right));
    }

    #[test]
    fn compare_json_string_lt_less() {
        // Kills String Lt `< -> >`: "a" < "b" is true; "a" > "b" is false.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("b".into());
        assert!(compare_json(&left, CompiledCompareOp::Lt, &right));
    }

    #[test]
    fn compare_json_string_le_equal_inputs() {
        // Kills String Le `<= -> >`: equal strings are Le-true, not Gt.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("a".into());
        assert!(compare_json(&left, CompiledCompareOp::Le, &right));
    }

    #[test]
    fn compare_json_string_gt_equal_inputs() {
        // Kills String Gt `> -> ==` and `> -> >=`: equal strings are not Gt.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("a".into());
        assert!(!compare_json(&left, CompiledCompareOp::Gt, &right));
    }

    #[test]
    fn compare_json_string_gt_greater() {
        // Kills String Gt `> -> <`: "b" > "a" is true; "b" < "a" is false.
        let left = serde_json::Value::String("b".into());
        let right = serde_json::Value::String("a".into());
        assert!(compare_json(&left, CompiledCompareOp::Gt, &right));
    }

    #[test]
    fn compare_json_string_ge_equal_inputs() {
        // Kills String Ge `>= -> <`: equal strings are Ge-true, not Lt.
        let left = serde_json::Value::String("a".into());
        let right = serde_json::Value::String("a".into());
        assert!(compare_json(&left, CompiledCompareOp::Ge, &right));
    }

    #[test]
    fn compare_json_bool_ne_different() {
        // Kills both Bool Ne mutants: `delete match arm Ne` (would fall through
        // to `_ => false`) and `!= -> ==` (would yield `true == false` = false).
        let left = serde_json::Value::Bool(true);
        let right = serde_json::Value::Bool(false);
        assert!(compare_json(&left, CompiledCompareOp::Ne, &right));
    }

    // ---- Phase-output cross-phase field access (issue #196) ----

    fn verify_output() -> PhaseOutput {
        PhaseOutput::new()
            .with_completed(true)
            .with_outcome("error")
            .with_error_count(2)
            .with_warning_count(1)
    }

    #[test]
    fn resolves_phase_outcome_field() {
        let out = verify_output();
        let lookup =
            move |name: &str| -> Option<PhaseOutput> { (name == "verify").then(|| out.clone()) };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        let v = resolve_phase_field(&ctx, "verify", &["outcome".to_string()]).unwrap();
        assert_eq!(v, serde_json::Value::String("error".into()));
    }

    #[test]
    fn resolves_phase_completed_and_modified() {
        let out = verify_output();
        let lookup =
            move |name: &str| -> Option<PhaseOutput> { (name == "verify").then(|| out.clone()) };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        assert_eq!(
            resolve_phase_field(&ctx, "verify", &["completed".to_string()]),
            Some(serde_json::Value::Bool(true))
        );
        assert_eq!(
            resolve_phase_field(&ctx, "verify", &["modified".to_string()]),
            Some(serde_json::Value::Bool(false))
        );
    }

    #[test]
    fn resolves_phase_counts() {
        let out = verify_output();
        let lookup =
            move |name: &str| -> Option<PhaseOutput> { (name == "verify").then(|| out.clone()) };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        assert_eq!(
            resolve_phase_field(&ctx, "verify", &["error_count".to_string()]),
            Some(serde_json::json!(2))
        );
        assert_eq!(
            resolve_phase_field(&ctx, "verify", &["warning_count".to_string()]),
            Some(serde_json::json!(1))
        );
    }

    #[test]
    fn unknown_phase_field_returns_none() {
        let out = verify_output();
        let lookup =
            move |name: &str| -> Option<PhaseOutput> { (name == "verify").then(|| out.clone()) };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        assert!(resolve_phase_field(&ctx, "verify", &["bogus".to_string()]).is_none());
    }

    #[test]
    fn unknown_phase_returns_none() {
        let lookup = |_: &str| -> Option<PhaseOutput> { None };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        assert!(resolve_phase_field(&ctx, "missing", &["outcome".to_string()]).is_none());
    }

    #[test]
    fn phase_field_without_lookup_returns_none() {
        let ctx = EvalContext::empty();
        assert!(resolve_phase_field(&ctx, "verify", &["outcome".to_string()]).is_none());
    }

    #[test]
    fn phase_outcome_drives_field_compare_skip() {
        // Full-stack: a FieldCompare against a phase output should resolve
        // through resolve_field's fallback branch.
        let file = test_file();
        let out = PhaseOutput::new()
            .with_completed(true)
            .with_outcome("error");
        let lookup =
            move |name: &str| -> Option<PhaseOutput> { (name == "verify").then(|| out.clone()) };
        let ctx = EvalContext::with_phase_outputs(&lookup);
        // verify.outcome != "ok" should be true because outcome == "error".
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["verify".into(), "outcome".into()],
                op: CompiledCompareOp::Ne,
                value: serde_json::Value::String("ok".into()),
            },
            &file,
            &ctx,
        ));
        // verify.outcome == "error" should be true.
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["verify".into(), "outcome".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("error".into()),
            },
            &file,
            &ctx,
        ));
    }
}
