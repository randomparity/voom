use serde::Deserialize;

pub(super) const AUDIO_LANGUAGE_DETECTOR_PLUGIN: &str = "audio-language-detector";

#[derive(Debug, Deserialize)]
struct AudioLanguageMetadata {
    #[serde(default)]
    detections: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AudioLanguageDetection {
    track_index: u32,
    detected_language: String,
}

/// Apply audio language detection results to track language fields.
///
/// This translates the `audio-language-detector` metadata contract into track
/// mutations before policy evaluation, keeping plugin-specific JSON validation
/// out of the process pipeline.
pub(super) fn apply_detected_languages(file: &mut voom_domain::media::MediaFile) {
    let Some(metadata) = file.plugin_metadata.get(AUDIO_LANGUAGE_DETECTOR_PLUGIN) else {
        return;
    };

    let metadata = match serde_json::from_value::<AudioLanguageMetadata>(metadata.clone()) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(
                path = %file.path.display(),
                error = %error,
                "audio language detector metadata has an unexpected shape"
            );
            return;
        }
    };

    for detection in metadata.detections {
        let detection = match serde_json::from_value::<AudioLanguageDetection>(detection) {
            Ok(detection) => detection,
            Err(error) => {
                tracing::warn!(
                    path = %file.path.display(),
                    error = %error,
                    "audio language detector detection has an unexpected shape"
                );
                continue;
            }
        };
        apply_detection(file, detection);
    }
}

fn apply_detection(file: &mut voom_domain::media::MediaFile, detection: AudioLanguageDetection) {
    let Some(track) = file
        .tracks
        .iter_mut()
        .find(|track| track.index == detection.track_index)
    else {
        return;
    };

    let Some(normalized) =
        voom_domain::utils::language::normalize_language(&detection.detected_language)
    else {
        tracing::warn!(
            path = %file.path.display(),
            track = detection.track_index,
            detected = %detection.detected_language,
            "unrecognized language code from detector, skipping"
        );
        return;
    };

    if track.language == normalized {
        return;
    }

    if track.language != "und" {
        tracing::warn!(
            path = %file.path.display(),
            track = detection.track_index,
            existing = %track.language,
            detected = %normalized,
            "overwriting track language with detected value"
        );
    }

    track.language = normalized.to_string();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::media::{MediaFile, Track, TrackType};

    use super::{AUDIO_LANGUAGE_DETECTOR_PLUGIN, apply_detected_languages};

    fn make_file_with_audio_tracks() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/tmp/test.mkv"));
        let mut main = Track::new(0, TrackType::AudioMain, "aac".into());
        main.language = "und".to_string();
        let mut alternate = Track::new(1, TrackType::AudioAlternate, "ac3".into());
        alternate.language = "fre".to_string();
        file.tracks = vec![main, alternate];
        file
    }

    fn set_detector_metadata(file: &mut MediaFile, metadata: serde_json::Value) {
        file.plugin_metadata
            .insert(AUDIO_LANGUAGE_DETECTOR_PLUGIN.to_string(), metadata);
    }

    #[test]
    fn applies_detected_language_to_unknown_track_language() {
        let mut file = make_file_with_audio_tracks();
        set_detector_metadata(
            &mut file,
            serde_json::json!({
                "detections": [{
                    "track_index": 0,
                    "detected_language": "eng",
                    "confidence": 0.95
                }]
            }),
        );

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[0].language, "eng");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn overwrites_existing_mismatched_language() {
        let mut file = make_file_with_audio_tracks();
        set_detector_metadata(
            &mut file,
            serde_json::json!({
                "detections": [{
                    "track_index": 1,
                    "detected_language": "eng",
                    "confidence": 0.92
                }]
            }),
        );

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[1].language, "eng");
    }

    #[test]
    fn applies_special_language_codes() {
        for language in ["zxx", "mul"] {
            let mut file = make_file_with_audio_tracks();
            set_detector_metadata(
                &mut file,
                serde_json::json!({
                    "detections": [{
                        "track_index": 0,
                        "detected_language": language,
                        "confidence": 0.98
                    }]
                }),
            );

            apply_detected_languages(&mut file);

            assert_eq!(file.tracks[0].language, language);
        }
    }

    #[test]
    fn missing_metadata_leaves_tracks_unchanged() {
        let mut file = make_file_with_audio_tracks();

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[0].language, "und");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn malformed_metadata_leaves_tracks_unchanged() {
        let mut file = make_file_with_audio_tracks();
        set_detector_metadata(
            &mut file,
            serde_json::json!({
                "detections": [{
                    "track_index": "not-an-index",
                    "detected_language": "eng"
                }]
            }),
        );

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[0].language, "und");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn malformed_detection_does_not_block_valid_detection() {
        let mut file = make_file_with_audio_tracks();
        set_detector_metadata(
            &mut file,
            serde_json::json!({
                "detections": [
                    {
                        "track_index": "not-an-index",
                        "detected_language": "eng"
                    },
                    {
                        "track_index": 0,
                        "detected_language": "eng"
                    }
                ]
            }),
        );

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[0].language, "eng");
        assert_eq!(file.tracks[1].language, "fre");
    }

    #[test]
    fn unknown_track_detection_leaves_tracks_unchanged() {
        let mut file = make_file_with_audio_tracks();
        set_detector_metadata(
            &mut file,
            serde_json::json!({
                "detections": [{
                    "track_index": 99,
                    "detected_language": "eng",
                    "confidence": 0.95
                }]
            }),
        );

        apply_detected_languages(&mut file);

        assert_eq!(file.tracks[0].language, "und");
        assert_eq!(file.tracks[1].language, "fre");
    }
}
