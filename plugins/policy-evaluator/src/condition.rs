//! Condition evaluation: evaluates `CompiledCondition` against a `MediaFile`.

use std::collections::HashSet;

use voom_domain::capability_map::CapabilityMap;
use voom_domain::media::MediaFile;
use voom_dsl::compiled::{CompiledCompareOp, CompiledCondition};

use crate::filter::{compare_f64, track_matches_with_context, tracks_for_target};

/// Evaluation context carrying system-level information (e.g. hwaccel
/// capabilities) into condition evaluation.
pub struct EvalContext<'a> {
    pub capabilities: Option<&'a CapabilityMap>,
}

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

/// Resolve a field path against the media file (and optionally system context).
pub(crate) fn resolve_field(
    file: &MediaFile,
    path: &[String],
    ctx: &EvalContext<'_>,
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }

    match path[0].as_str() {
        "video" => resolve_video_field(file, &path[1..]),
        "audio" => resolve_audio_field(file, &path[1..]),
        "plugin" => resolve_plugin_field(file, &path[1..]),
        "file" => resolve_file_field(file, &path[1..]),
        "system" => resolve_system_field(&path[1..], ctx),
        _ => None,
    }
}

/// Resolve `system.*` fields from the capability map.
fn resolve_system_field(path: &[String], ctx: &EvalContext<'_>) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let caps = ctx.capabilities?;
    match path[0].as_str() {
        "hwaccel" => Some(serde_json::Value::String(caps.best_hwaccel().to_string())),
        "has_hwaccel" => Some(serde_json::json!(caps.best_hwaccel() != "none")),
        "hwaccels" => {
            let accels: Vec<serde_json::Value> = caps
                .hw_accels()
                .into_iter()
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect();
            Some(serde_json::Value::Array(accels))
        }
        _ => None,
    }
}

fn resolve_video_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    let video = file.video_tracks().into_iter().next()?;
    resolve_track_field(video, path)
}

fn resolve_audio_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    let audio = file.audio_tracks().into_iter().next()?;
    resolve_track_field(audio, path)
}

fn resolve_track_field(
    track: &voom_domain::media::Track,
    path: &[String],
) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    match path[0].as_str() {
        "codec" => Some(serde_json::Value::String(track.codec.clone())),
        "language" | "lang" => Some(serde_json::Value::String(track.language.clone())),
        "title" => Some(serde_json::Value::String(track.title.clone())),
        "channels" => track.channels.map(|c| serde_json::json!(c)),
        "width" => track.width.map(|w| serde_json::json!(w)),
        "height" => track.height.map(|h| serde_json::json!(h)),
        "frame_rate" => track.frame_rate.map(|f| serde_json::json!(f)),
        "is_default" => Some(serde_json::json!(track.is_default)),
        "is_forced" => Some(serde_json::json!(track.is_forced)),
        "is_hdr" => Some(serde_json::json!(track.is_hdr)),
        "is_vfr" => Some(serde_json::json!(track.is_vfr)),
        "hdr_format" => track
            .hdr_format
            .as_ref()
            .map(|f| serde_json::Value::String(f.clone())),
        _ => None,
    }
}

fn resolve_plugin_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    let plugin_data = file.plugin_metadata.get(&path[0])?;
    let mut current: &serde_json::Value = plugin_data;
    for key in &path[1..] {
        current = current.get(key)?;
    }
    Some(current.clone())
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

fn resolve_file_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }
    match path[0].as_str() {
        "container" => Some(serde_json::Value::String(file.container.as_str().into())),
        "size" => Some(serde_json::json!(file.size)),
        "duration" => Some(serde_json::json!(file.duration)),
        "path" => Some(serde_json::Value::String(
            file.path.to_string_lossy().into(),
        )),
        "filename" => file
            .path
            .file_name()
            .map(|n| serde_json::Value::String(n.to_string_lossy().into())),
        _ => file
            .tags
            .get(&path[0])
            .map(|v| serde_json::Value::String(v.clone())),
    }
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

/// Resolve a `CompiledValueOrField` to a concrete string value.
#[must_use]
pub fn resolve_value_or_field(
    vof: &voom_dsl::compiled::CompiledValueOrField,
    file: &MediaFile,
    ctx: &EvalContext<'_>,
) -> Option<String> {
    match vof {
        voom_dsl::compiled::CompiledValueOrField::Value(v) => match v {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        },
        voom_dsl::compiled::CompiledValueOrField::Field(path) => resolve_field(file, path, ctx)
            .map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{MediaFile, Track, TrackType};
    use voom_domain::test_support::test_media_file;
    use voom_dsl::compiled::TrackTarget;

    fn test_file() -> MediaFile {
        test_media_file()
    }

    fn no_ctx() -> EvalContext<'static> {
        EvalContext { capabilities: None }
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
        let ctx = EvalContext {
            capabilities: Some(&map),
        };

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
        let ctx = EvalContext {
            capabilities: Some(&map),
        };

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
        let ctx = EvalContext {
            capabilities: Some(&map),
        };

        assert!(evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["system".into(), "hwaccels".into()],
            },
            &file,
            &ctx,
        ));
    }
}
