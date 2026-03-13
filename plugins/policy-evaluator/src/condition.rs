//! Condition evaluation: evaluates `CompiledCondition` against a `MediaFile`.

use std::collections::HashSet;

use voom_domain::media::MediaFile;
use voom_dsl::compiler::{CompiledCompareOp, CompiledCondition, TrackTarget};

use crate::filter::{compare_f64, track_matches};

/// Evaluate a condition against a media file.
#[must_use] 
pub fn evaluate_condition(cond: &CompiledCondition, file: &MediaFile) -> bool {
    match cond {
        CompiledCondition::Exists { target, filter } => {
            let tracks = tracks_for_target(file, target);
            match filter {
                Some(f) => tracks.iter().any(|t| track_matches(t, f)),
                None => !tracks.is_empty(),
            }
        }
        CompiledCondition::Count {
            target,
            filter,
            op,
            value,
        } => {
            let tracks = tracks_for_target(file, target);
            let count = match filter {
                Some(f) => tracks.iter().filter(|t| track_matches(t, f)).count(),
                None => tracks.len(),
            };
            compare_f64(count as f64, op, *value)
        }
        CompiledCondition::FieldCompare { path, op, value } => {
            evaluate_field_compare(file, path, op, value)
        }
        CompiledCondition::FieldExists { path } => resolve_field(file, path).is_some(),
        CompiledCondition::AudioIsMultiLanguage => audio_lang_count(file) > 1,
        CompiledCondition::IsDubbed => {
            audio_lang_count(file) > 1 && !file.subtitle_tracks().is_empty()
        }
        CompiledCondition::IsOriginal => audio_lang_count(file) <= 1,
        CompiledCondition::And(conditions) => {
            conditions.iter().all(|c| evaluate_condition(c, file))
        }
        CompiledCondition::Or(conditions) => conditions.iter().any(|c| evaluate_condition(c, file)),
        CompiledCondition::Not(inner) => !evaluate_condition(inner, file),
    }
}

/// Resolve a field path against the media file.
fn resolve_field(file: &MediaFile, path: &[String]) -> Option<serde_json::Value> {
    if path.is_empty() {
        return None;
    }

    match path[0].as_str() {
        "video" => resolve_video_field(file, &path[1..]),
        "audio" => resolve_audio_field(file, &path[1..]),
        "plugin" => resolve_plugin_field(file, &path[1..]),
        "file" => resolve_file_field(file, &path[1..]),
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
    op: &CompiledCompareOp,
    value: &serde_json::Value,
) -> bool {
    let resolved = match resolve_field(file, path) {
        Some(v) => v,
        None => return false,
    };

    // Handle "In" operator: check if field value is in a list
    if *op == CompiledCompareOp::In {
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
    op: &CompiledCompareOp,
    right: &serde_json::Value,
) -> bool {
    match (left, right) {
        (serde_json::Value::Number(l), serde_json::Value::Number(r)) => {
            let lf = l.as_f64().unwrap_or(0.0);
            let rf = r.as_f64().unwrap_or(0.0);
            compare_f64(lf, op, rf)
        }
        (serde_json::Value::String(l), serde_json::Value::String(r)) => match op {
            CompiledCompareOp::Eq => l == r,
            CompiledCompareOp::Ne => l != r,
            CompiledCompareOp::Lt => l < r,
            CompiledCompareOp::Le => l <= r,
            CompiledCompareOp::Gt => l > r,
            CompiledCompareOp::Ge => l >= r,
            CompiledCompareOp::In => false,
        },
        (serde_json::Value::Bool(l), serde_json::Value::Bool(r)) => match op {
            CompiledCompareOp::Eq => l == r,
            CompiledCompareOp::Ne => l != r,
            _ => false,
        },
        _ => false,
    }
}

fn json_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    match (a, b) {
        (serde_json::Value::String(l), serde_json::Value::String(r)) => l == r,
        (serde_json::Value::Number(l), serde_json::Value::Number(r)) => l.as_f64() == r.as_f64(),
        (serde_json::Value::Bool(l), serde_json::Value::Bool(r)) => l == r,
        _ => a == b,
    }
}

/// Resolve a `CompiledValueOrField` to a concrete string value.
#[must_use] 
pub fn resolve_value_or_field(
    vof: &voom_dsl::compiler::CompiledValueOrField,
    file: &MediaFile,
) -> Option<String> {
    match vof {
        voom_dsl::compiler::CompiledValueOrField::Value(v) => match v {
            serde_json::Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        },
        voom_dsl::compiler::CompiledValueOrField::Field(path) => {
            resolve_field(file, path).map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
        }
    }
}

/// Get tracks from a file matching the given target type.
fn tracks_for_target<'a>(
    file: &'a MediaFile,
    target: &TrackTarget,
) -> Vec<&'a voom_domain::media::Track> {
    match target {
        TrackTarget::Video => file.video_tracks(),
        TrackTarget::Audio => file.audio_tracks(),
        TrackTarget::Subtitle => file.subtitle_tracks(),
        TrackTarget::Attachment => file.tracks_of_type(voom_domain::media::TrackType::Attachment),
        TrackTarget::Any => file.tracks.iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

    fn test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/test/movie.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![
            {
                let mut t = Track::new(0, TrackType::Video, "hevc".into());
                t.width = Some(1920);
                t.height = Some(1080);
                t
            },
            {
                let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
                t.language = "eng".into();
                t.channels = Some(6);
                t.is_default = true;
                t
            },
            {
                let mut t = Track::new(2, TrackType::AudioAlternate, "aac".into());
                t.language = "jpn".into();
                t.channels = Some(2);
                t
            },
            {
                let mut t = Track::new(3, TrackType::SubtitleMain, "srt".into());
                t.language = "eng".into();
                t
            },
        ];
        file
    }

    #[test]
    fn test_exists_condition() {
        let file = test_file();
        assert!(evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: None,
            },
            &file
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Attachment,
                filter: None,
            },
            &file
        ));
    }

    #[test]
    fn test_exists_with_filter() {
        let file = test_file();
        use voom_dsl::compiler::CompiledFilter;
        assert!(evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(CompiledFilter::LangIn(vec!["jpn".into()])),
            },
            &file
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(CompiledFilter::LangIn(vec!["fre".into()])),
            },
            &file
        ));
    }

    #[test]
    fn test_count_condition() {
        let file = test_file();
        assert!(evaluate_condition(
            &CompiledCondition::Count {
                target: TrackTarget::Audio,
                filter: None,
                op: CompiledCompareOp::Eq,
                value: 2.0,
            },
            &file
        ));
        assert!(evaluate_condition(
            &CompiledCondition::Count {
                target: TrackTarget::Audio,
                filter: None,
                op: CompiledCompareOp::Gt,
                value: 1.0,
            },
            &file
        ));
    }

    #[test]
    fn test_field_compare_video_codec() {
        let file = test_file();
        // video.codec == "hevc"
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::Eq,
                value: serde_json::Value::String("hevc".into()),
            },
            &file
        ));
    }

    #[test]
    fn test_field_compare_in_list() {
        let file = test_file();
        // video.codec in [hevc, h264]
        assert!(evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::In,
                value: serde_json::json!(["hevc", "h264"]),
            },
            &file
        ));
        // video.codec in [h264, vp9]
        assert!(!evaluate_condition(
            &CompiledCondition::FieldCompare {
                path: vec!["video".into(), "codec".into()],
                op: CompiledCompareOp::In,
                value: serde_json::json!(["h264", "vp9"]),
            },
            &file
        ));
    }

    #[test]
    fn test_field_exists() {
        let mut file = test_file();
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"original_language": "eng"}),
        );
        assert!(evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["plugin".into(), "radarr".into(), "original_language".into()],
            },
            &file
        ));
        assert!(!evaluate_condition(
            &CompiledCondition::FieldExists {
                path: vec!["plugin".into(), "sonarr".into(), "title".into()],
            },
            &file
        ));
    }

    #[test]
    fn test_audio_is_multi_language() {
        let file = test_file(); // eng + jpn
        assert!(evaluate_condition(
            &CompiledCondition::AudioIsMultiLanguage,
            &file
        ));

        let mut mono = MediaFile::new(PathBuf::from("/test.mkv"));
        mono.tracks = vec![{
            let mut t = Track::new(0, TrackType::AudioMain, "aac".into());
            t.language = "eng".into();
            t
        }];
        assert!(!evaluate_condition(
            &CompiledCondition::AudioIsMultiLanguage,
            &mono
        ));
    }

    #[test]
    fn test_and_or_not_conditions() {
        let file = test_file();
        // Has eng audio AND jpn audio
        let cond = CompiledCondition::And(vec![
            CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(voom_dsl::compiler::CompiledFilter::LangIn(vec![
                    "eng".into()
                ])),
            },
            CompiledCondition::Exists {
                target: TrackTarget::Audio,
                filter: Some(voom_dsl::compiler::CompiledFilter::LangIn(vec![
                    "jpn".into()
                ])),
            },
        ]);
        assert!(evaluate_condition(&cond, &file));

        // NOT has french audio
        let not_cond = CompiledCondition::Not(Box::new(CompiledCondition::Exists {
            target: TrackTarget::Audio,
            filter: Some(voom_dsl::compiler::CompiledFilter::LangIn(vec![
                "fre".into()
            ])),
        }));
        assert!(evaluate_condition(&not_cond, &file));
    }

    #[test]
    fn test_resolve_plugin_field() {
        let mut file = test_file();
        file.plugin_metadata.insert(
            "radarr".into(),
            serde_json::json!({"title": "Test Movie", "original_language": "eng"}),
        );

        let val = resolve_value_or_field(
            &voom_dsl::compiler::CompiledValueOrField::Field(vec![
                "plugin".into(),
                "radarr".into(),
                "title".into(),
            ]),
            &file,
        );
        assert_eq!(val, Some("Test Movie".into()));
    }

    #[test]
    fn test_resolve_value() {
        let file = test_file();
        let val = resolve_value_or_field(
            &voom_dsl::compiler::CompiledValueOrField::Value(serde_json::Value::String(
                "literal".into(),
            )),
            &file,
        );
        assert_eq!(val, Some("literal".into()));
    }
}
