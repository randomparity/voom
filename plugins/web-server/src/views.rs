//! View structs that wrap domain types with computed fields for templates.

use serde::Serialize;
use voom_domain::media::MediaFile;
use voom_domain::utils::datetime::{format_duration, format_size};

/// A template-friendly view of a `MediaFile` with computed display fields.
#[derive(Debug, Serialize)]
pub struct FileView {
    /// Extracted filename from path.
    pub filename: String,
    /// Human-readable file size (e.g., "1.23 GiB").
    pub size_human: String,
    /// Number of tracks.
    pub track_count: usize,
    /// Human-readable duration (e.g., "1h 23m 45s").
    pub duration_human: String,
    /// All other MediaFile fields.
    #[serde(flatten)]
    pub file: MediaFile,
}

impl FileView {
    #[must_use]
    pub fn from_media_file(file: MediaFile) -> Self {
        let filename = file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());
        let size_human = format_size(file.size);
        let track_count = file.tracks.len();
        let duration_human = format_duration(file.duration);

        Self {
            filename,
            size_human,
            track_count,
            duration_human,
            file,
        }
    }
}

/// Convert a list of `MediaFile` into `FileView` for template rendering.
#[must_use]
pub fn file_views(files: Vec<MediaFile>) -> Vec<FileView> {
    files.into_iter().map(FileView::from_media_file).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{Container, Track, TrackType};

    #[test]
    fn file_view_extracts_filename() {
        let file = MediaFile::new(PathBuf::from("/media/movies/Test Movie.mkv"));
        let view = FileView::from_media_file(file);
        assert_eq!(view.filename, "Test Movie.mkv");
    }

    #[test]
    fn file_view_computes_size_human() {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.size = 2_500_000_000;
        let view = FileView::from_media_file(file);
        assert_eq!(view.size_human, "2.33 GiB");
    }

    #[test]
    fn file_view_computes_track_count() {
        let file = MediaFile::new(PathBuf::from("/test.mkv")).with_tracks(vec![
            Track::new(0, TrackType::Video, "hevc".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
        ]);
        let view = FileView::from_media_file(file);
        assert_eq!(view.track_count, 2);
    }

    #[test]
    fn file_view_computes_duration_human() {
        let file = MediaFile::new(PathBuf::from("/test.mkv")).with_duration(5432.0);
        let view = FileView::from_media_file(file);
        assert_eq!(view.duration_human, "1h 30m 32s");
    }

    #[test]
    fn file_views_converts_vec() {
        let files = vec![
            MediaFile::new(PathBuf::from("/a.mkv")),
            MediaFile::new(PathBuf::from("/b.mp4")),
        ];
        let views = file_views(files);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].filename, "a.mkv");
        assert_eq!(views[1].filename, "b.mp4");
    }

    #[test]
    fn file_view_serializes_with_flattened_fields() {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.container = Container::Mkv;
        file.size = 1024;
        let view = FileView::from_media_file(file);
        let json = serde_json::to_value(&view).unwrap();
        // Computed fields
        assert_eq!(json["filename"], "test.mkv");
        assert_eq!(json["size_human"], "1 KiB");
        assert_eq!(json["track_count"], 0);
        // Flattened fields from MediaFile
        assert!(json["id"].is_string());
        assert!(json["path"].is_string());
        assert_eq!(json["container"], "Mkv");
    }
}
