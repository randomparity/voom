//! View structs that wrap domain types with computed fields for templates.

use serde::Serialize;
use voom_domain::media::MediaFile;
use voom_domain::utils::format::{format_duration, format_size};
use voom_domain::verification::{VerificationMode, VerificationRecord};

/// A template-friendly view of a `MediaFile` with computed display fields.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct FileView {
    /// Extracted filename from path.
    pub filename: String,
    /// Human-readable file size (e.g., "1.23 GiB").
    pub size_human: String,
    /// Number of tracks.
    pub track_count: usize,
    /// Human-readable duration (e.g., "1h 23m 45s").
    pub duration_human: String,
    /// All other `MediaFile` fields, flattened into the same JSON object.
    /// Computed field names (`filename`, `size_human`, `track_count`, `duration_human`)
    /// must not collide with `MediaFile` field names.
    #[serde(flatten)]
    pub file: MediaFile,
}

impl FileView {
    #[must_use]
    pub fn from_media_file(file: MediaFile) -> Self {
        let filename = file.path.file_name().map_or_else(
            || "unknown".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
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

use voom_domain::transition::FileTransition;

/// A template-friendly view of a `FileTransition` with computed display fields.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct TransitionView {
    /// Human-readable file size after the transition.
    pub to_size_human: String,
    /// Human-readable file size before the transition (if known).
    pub from_size_human: Option<String>,
    /// Human-readable size delta (e.g., "-150.00 MiB" or "+25 KiB").
    pub size_delta: Option<String>,
    /// Whether the size change was a reduction (for CSS styling).
    pub size_decreased: bool,
    /// The source as a lowercase display string.
    pub source_label: String,
    /// Human-readable processing duration (e.g., "1.5s", "2m 03s").
    pub duration_human: Option<String>,
    /// All original `FileTransition` fields, flattened.
    #[serde(flatten)]
    pub transition: FileTransition,
}

impl TransitionView {
    #[must_use]
    pub fn from_transition(t: FileTransition) -> Self {
        let to_size_human = format_size(t.to_size);
        let from_size_human = t.from_size.map(format_size);
        let size_delta = t.from_size.map(|from| format_size_delta(from, t.to_size));
        let size_decreased = t.from_size.is_some_and(|from| t.to_size < from);
        let source_label = t.source.as_str().to_string();
        let duration_human = t.duration_ms.map(format_duration_ms);

        Self {
            to_size_human,
            from_size_human,
            size_delta,
            size_decreased,
            source_label,
            duration_human,
            transition: t,
        }
    }
}

/// Convert a list of `FileTransition` into `TransitionView` for template rendering.
#[must_use]
pub fn transition_views(transitions: Vec<FileTransition>) -> Vec<TransitionView> {
    transitions
        .into_iter()
        .map(TransitionView::from_transition)
        .collect()
}

/// A template-friendly view of a verification record.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct VerificationView {
    /// Lowercase verification mode label.
    pub mode_label: String,
    /// Lowercase outcome label.
    pub outcome_label: String,
    /// True when the result is a hash verification failure.
    pub is_hash_mismatch: bool,
    /// All original `VerificationRecord` fields, flattened.
    #[serde(flatten)]
    pub verification: VerificationRecord,
}

impl VerificationView {
    #[must_use]
    pub fn from_verification(verification: VerificationRecord) -> Self {
        let mode_label = verification.mode.as_str().to_string();
        let outcome_label = verification.outcome.as_str().to_string();
        let is_hash_mismatch =
            verification.mode == VerificationMode::Hash && verification.error_count > 0;

        Self {
            mode_label,
            outcome_label,
            is_hash_mismatch,
            verification,
        }
    }
}

/// Convert verification records into template views.
#[must_use]
pub fn verification_views(records: Vec<VerificationRecord>) -> Vec<VerificationView> {
    records
        .into_iter()
        .map(VerificationView::from_verification)
        .collect()
}

/// A failing file row for the integrity page.
#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct IntegrityErrorView {
    /// File metadata and computed display fields.
    pub file: FileView,
    /// Latest error verification for the file.
    pub verification: VerificationView,
}

impl IntegrityErrorView {
    #[must_use]
    pub fn new(file: MediaFile, verification: VerificationRecord) -> Self {
        Self {
            file: FileView::from_media_file(file),
            verification: VerificationView::from_verification(verification),
        }
    }
}

/// Format a size delta as a human-readable signed string.
fn format_size_delta(from: u64, to: u64) -> String {
    let (sign, abs) = if to < from {
        ("-", from - to)
    } else {
        ("+", to - from)
    };
    format!("{sign}{}", format_size(abs))
}

/// Format milliseconds into a human-readable duration string.
fn format_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let total_secs = ms as f64 / 1000.0;
    if total_secs < 60.0 {
        return format!("{total_secs:.1}s");
    }
    let mins = ms / 60_000;
    let secs = (ms % 60_000) / 1000;
    format!("{mins}m {secs:02}s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;
    use voom_domain::media::{Container, Track, TrackType};

    #[test]
    fn test_file_view_extracts_filename() {
        let file = MediaFile::new(PathBuf::from("/media/movies/Test Movie.mkv"));
        let view = FileView::from_media_file(file);
        assert_eq!(view.filename, "Test Movie.mkv");
    }

    #[test]
    fn test_file_view_computes_size_human() {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.size = 2_500_000_000;
        let view = FileView::from_media_file(file);
        assert_eq!(view.size_human, "2.33 GiB");
    }

    #[test]
    fn test_file_view_computes_track_count() {
        let file = MediaFile::new(PathBuf::from("/test.mkv")).with_tracks(vec![
            Track::new(0, TrackType::Video, "hevc".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
        ]);
        let view = FileView::from_media_file(file);
        assert_eq!(view.track_count, 2);
    }

    #[test]
    fn test_file_view_computes_duration_human() {
        let file = MediaFile::new(PathBuf::from("/test.mkv")).with_duration(5432.0);
        let view = FileView::from_media_file(file);
        assert_eq!(view.duration_human, "1h 30m 32s");
    }

    #[test]
    fn test_file_views_converts_vec() {
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
    fn test_file_view_serializes_with_flattened_fields() {
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

    use voom_domain::transition::{FileTransition, TransitionSource};

    #[test]
    fn test_transition_view_computes_fields() {
        let t = FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/media/movie.mkv"),
            "newhash".into(),
            2_000_000_000,
            TransitionSource::Voom,
        )
        .with_from(Some("oldhash".into()), Some(3_000_000_000))
        .with_detail("mkvtoolnix")
        .with_processing(
            1500,
            3,
            2,
            voom_domain::stats::ProcessingOutcome::Success,
            "default",
            "normalize",
        );

        let view = TransitionView::from_transition(t);
        assert_eq!(view.to_size_human, "1.86 GiB");
        assert_eq!(view.from_size_human, Some("2.79 GiB".to_string()));
        assert_eq!(view.size_delta, Some("-953.7 MiB".to_string()));
        assert_eq!(view.source_label, "voom");
        assert_eq!(view.duration_human, Some("1.5s".to_string()));
    }

    #[test]
    fn test_transition_view_discovery_no_prior() {
        let t = FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/media/movie.mkv"),
            "hash1".into(),
            500_000,
            TransitionSource::Discovery,
        );

        let view = TransitionView::from_transition(t);
        assert_eq!(view.to_size_human, "488 KiB");
        assert!(view.from_size_human.is_none());
        assert!(view.size_delta.is_none());
        assert_eq!(view.source_label, "discovery");
        assert!(view.duration_human.is_none());
    }

    #[test]
    fn test_transition_views_converts_vec() {
        let transitions = vec![
            FileTransition::new(
                Uuid::new_v4(),
                PathBuf::from("/a.mkv"),
                "h1".into(),
                1000,
                TransitionSource::Discovery,
            ),
            FileTransition::new(
                Uuid::new_v4(),
                PathBuf::from("/a.mkv"),
                "h2".into(),
                2000,
                TransitionSource::External,
            ),
        ];
        let views = transition_views(transitions);
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].source_label, "discovery");
        assert_eq!(views[1].source_label, "external");
    }
}
