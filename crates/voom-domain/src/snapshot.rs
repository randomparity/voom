//! Lightweight media state summary captured at transition boundaries.

use serde::{Deserialize, Serialize};

use crate::media::{CropRect, MediaFile};

/// A lightweight summary of a file's media state at a point in time.
///
/// Stored as JSON in the `metadata_snapshot` column of `file_transitions` so
/// that history views can show what the file looked like before and after each
/// processing step without re-reading the full `MediaFile` record.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetadataSnapshot {
    pub container: String,
    pub video_tracks: u32,
    pub audio_tracks: u32,
    pub subtitle_tracks: u32,
    /// Unique codecs present across all tracks, sorted alphabetically.
    pub codecs: Vec<String>,
    /// Resolution of the first video track, e.g. `"3840x2160"`.
    pub resolution: Option<String>,
    /// Cached crop rectangle, expressed as pixels removed from each source edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropRect>,
    pub duration_secs: f64,
}

impl MetadataSnapshot {
    /// Build a snapshot from a [`MediaFile`].
    #[must_use]
    pub fn from_media_file(file: &MediaFile) -> Self {
        let video_tracks = file.video_tracks();
        let audio_tracks = file.audio_tracks();
        let subtitle_tracks = file.subtitle_tracks();

        let resolution = video_tracks
            .first()
            .and_then(|t| match (t.width, t.height) {
                (Some(w), Some(h)) => Some(format!("{w}x{h}")),
                (None, _) | (_, None) => None,
            });

        let mut codecs = Vec::new();
        for track in &file.tracks {
            let codec = track.codec.trim().to_string();
            if !codec.is_empty() {
                codecs.push(codec);
            }
        }
        codecs.sort_unstable();
        codecs.dedup();

        Self {
            container: file.container.as_str().to_string(),
            video_tracks: u32::try_from(video_tracks.len()).unwrap_or(u32::MAX),
            audio_tracks: u32::try_from(audio_tracks.len()).unwrap_or(u32::MAX),
            subtitle_tracks: u32::try_from(subtitle_tracks.len()).unwrap_or(u32::MAX),
            codecs,
            resolution,
            crop: file.crop_detection.as_ref().map(|detection| detection.rect),
            duration_secs: if file.duration.is_finite() {
                file.duration
            } else {
                0.0
            },
        }
    }

    /// Serialize to a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if serialization fails (only possible for
    /// non-finite floats, which `f64` can hold but JSON cannot represent).
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }

    /// Deserialize from a JSON string.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if the string is not valid JSON or does
    /// not match the expected shape.
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use chrono::Utc;

    use crate::media::{Container, CropDetection, CropRect, MediaFile, Track, TrackType};

    fn video_track(index: u32, codec: &str, width: u32, height: u32) -> Track {
        Track {
            width: Some(width),
            height: Some(height),
            ..Track::new(index, TrackType::Video, codec.to_string())
        }
    }

    fn audio_track(index: u32, codec: &str) -> Track {
        Track::new(index, TrackType::AudioMain, codec.to_string())
    }

    fn subtitle_track(index: u32, codec: &str) -> Track {
        Track::new(index, TrackType::SubtitleMain, codec.to_string())
    }

    #[test]
    fn snapshot_from_typical_media_file() {
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7320.5)
            .with_tracks(vec![
                video_track(0, "hevc", 3840, 2160),
                audio_track(1, "truehd"),
                audio_track(2, "aac"),
                subtitle_track(3, "srt"),
                subtitle_track(4, "srt"),
            ]);

        let snap = MetadataSnapshot::from_media_file(&file);

        assert_eq!(snap.container, "mkv");
        assert_eq!(snap.video_tracks, 1);
        assert_eq!(snap.audio_tracks, 2);
        assert_eq!(snap.subtitle_tracks, 2);
        assert_eq!(snap.resolution, Some("3840x2160".to_string()));
        assert_eq!(snap.crop, None);
        assert_eq!(snap.duration_secs, 7320.5);
        // codecs sorted and deduped: aac, hevc, srt (two srt tracks → one entry), truehd
        assert_eq!(snap.codecs, vec!["aac", "hevc", "srt", "truehd"]);
    }

    #[test]
    fn snapshot_from_audio_only_file() {
        let file = MediaFile::new(PathBuf::from("/music/album.flac"))
            .with_container(Container::Other)
            .with_duration(210.0)
            .with_tracks(vec![audio_track(0, "flac")]);

        let snap = MetadataSnapshot::from_media_file(&file);

        assert_eq!(snap.video_tracks, 0);
        assert_eq!(snap.audio_tracks, 1);
        assert_eq!(snap.subtitle_tracks, 0);
        assert_eq!(snap.resolution, None);
        assert_eq!(snap.codecs, vec!["flac"]);
    }

    #[test]
    fn snapshot_includes_crop_detection() {
        let mut file = MediaFile::new(PathBuf::from("/movies/cropped.mkv"))
            .with_container(Container::Mkv)
            .with_duration(5400.0)
            .with_tracks(vec![video_track(0, "h264", 1920, 1080)]);
        file.crop_detection = Some(CropDetection::new(
            CropRect::new(0, 132, 0, 132),
            Utc::now(),
        ));

        let snap = MetadataSnapshot::from_media_file(&file);

        assert_eq!(snap.crop, Some(CropRect::new(0, 132, 0, 132)));
    }

    #[test]
    fn snapshot_from_empty_file() {
        let file = MediaFile::new(PathBuf::from("/empty.mkv"));

        let snap = MetadataSnapshot::from_media_file(&file);

        assert_eq!(snap.video_tracks, 0);
        assert_eq!(snap.audio_tracks, 0);
        assert_eq!(snap.subtitle_tracks, 0);
        assert!(snap.codecs.is_empty());
        assert_eq!(snap.resolution, None);
        assert_eq!(snap.duration_secs, 0.0);
    }

    #[test]
    fn snapshot_json_roundtrip() {
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(5400.0)
            .with_tracks(vec![
                video_track(0, "h264", 1920, 1080),
                audio_track(1, "aac"),
            ]);

        let snap = MetadataSnapshot::from_media_file(&file);
        let json = snap.to_json().expect("serialization should succeed");
        let restored = MetadataSnapshot::from_json(&json).expect("deserialization should succeed");

        assert_eq!(snap, restored);
    }

    #[test]
    fn snapshot_json_omits_missing_crop() {
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(5400.0)
            .with_tracks(vec![video_track(0, "h264", 1920, 1080)]);

        let json = MetadataSnapshot::from_media_file(&file)
            .to_json()
            .expect("serialization should succeed");

        assert!(
            !json.contains("crop"),
            "missing crop should not be serialized: {json}"
        );
    }

    #[test]
    fn snapshot_deserializes_legacy_json_without_crop() {
        let snap = MetadataSnapshot::from_json(
            r#"{
                "container": "mkv",
                "video_tracks": 1,
                "audio_tracks": 2,
                "subtitle_tracks": 0,
                "codecs": ["aac", "h264"],
                "resolution": "1920x1080",
                "duration_secs": 5400.0
            }"#,
        )
        .expect("legacy snapshot JSON should deserialize");

        assert_eq!(snap.crop, None);
    }

    #[test]
    fn snapshot_deduplicates_codecs() {
        let file = MediaFile::new(PathBuf::from("/movies/multi.mkv"))
            .with_container(Container::Mkv)
            .with_duration(3600.0)
            .with_tracks(vec![audio_track(0, "aac"), audio_track(1, "aac")]);

        let snap = MetadataSnapshot::from_media_file(&file);

        assert_eq!(snap.codecs, vec!["aac"]);
    }

    #[test]
    fn snapshot_clamps_nan_duration() {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.duration = f64::NAN;

        let snap = MetadataSnapshot::from_media_file(&file);
        assert_eq!(snap.duration_secs, 0.0);
        snap.to_json()
            .expect("NaN was clamped, so JSON should succeed");
    }

    #[test]
    fn snapshot_clamps_infinite_duration() {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.duration = f64::INFINITY;

        let snap = MetadataSnapshot::from_media_file(&file);
        assert_eq!(snap.duration_secs, 0.0);
        snap.to_json()
            .expect("Infinity was clamped, so JSON should succeed");
    }
}
