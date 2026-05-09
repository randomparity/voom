use serde::Deserialize;

pub(super) const AUDIO_LANGUAGE_DETECTOR_PLUGIN: &str = "audio-language-detector";

#[derive(Deserialize)]
struct AudioLanguageMetadata {
    #[serde(default)]
    detections: Vec<AudioLanguageDetection>,
}

#[derive(Deserialize)]
struct AudioLanguageDetection {
    track_index: u32,
    detected_language: String,
}

/// Apply audio language detection results to track language fields.
///
/// This translates the `audio-language-detector` metadata contract into
/// track mutations before policy evaluation, so the process pipeline does not
/// parse plugin-specific JSON directly.
pub(super) fn apply_detected_languages(file: &mut voom_domain::media::MediaFile) {
    let Some(metadata) = file.plugin_metadata.get(AUDIO_LANGUAGE_DETECTOR_PLUGIN) else {
        return;
    };

    let Ok(metadata) = serde_json::from_value::<AudioLanguageMetadata>(metadata.clone()) else {
        tracing::warn!(
            path = %file.path.display(),
            "audio language detector metadata has an unexpected shape"
        );
        return;
    };

    for detection in metadata.detections {
        let Some(track) = file
            .tracks
            .iter_mut()
            .find(|t| t.index == detection.track_index)
        else {
            continue;
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
            continue;
        };

        if track.language == normalized {
            continue;
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
}
