use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Outcome of processing a file through a policy phase.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingOutcome {
    #[default]
    Success,
    Failure,
    Skipped,
}

impl ProcessingOutcome {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessingOutcome::Success => "success",
            ProcessingOutcome::Failure => "failure",
            ProcessingOutcome::Skipped => "skipped",
        }
    }

    /// Parse from a string stored in the database.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" | "completed" => Some(ProcessingOutcome::Success),
            "failure" | "failed" => Some(ProcessingOutcome::Failure),
            "skipped" => Some(ProcessingOutcome::Skipped),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProcessingOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What triggered a library snapshot capture.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTrigger {
    ScanComplete,
    IntrospectComplete,
    Manual,
}

impl SnapshotTrigger {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SnapshotTrigger::ScanComplete => "scan_complete",
            SnapshotTrigger::IntrospectComplete => "introspect_complete",
            SnapshotTrigger::Manual => "manual",
        }
    }
}

impl std::fmt::Display for SnapshotTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A point-in-time snapshot of aggregate library statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibrarySnapshot {
    pub id: Uuid,
    pub captured_at: DateTime<Utc>,
    pub trigger: SnapshotTrigger,
    pub files: FileStats,
    pub video: VideoStats,
    pub audio: AudioStats,
    pub subtitles: SubtitleStats,
    pub processing: ProcessingAggregateStats,
    pub jobs: JobAggregateStats,
}

/// Aggregate file-level statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileStats {
    pub total_count: u64,
    pub total_size_bytes: u64,
    pub total_duration_secs: f64,
    pub avg_size_bytes: u64,
    pub avg_duration_secs: f64,
    pub max_size_bytes: u64,
    pub min_size_bytes: u64,
    pub container_counts: Vec<(String, u64)>,
}

/// Aggregate video track statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VideoStats {
    pub total_tracks: u64,
    pub codec_counts: Vec<(String, u64)>,
    pub resolution_counts: Vec<(String, u64)>,
    pub hdr_count: u64,
    pub hdr_format_counts: Vec<(String, u64)>,
    pub frame_rate_counts: Vec<(String, u64)>,
    pub vfr_count: u64,
    pub pixel_format_counts: Vec<(String, u64)>,
}

/// Aggregate audio track statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioStats {
    pub total_tracks: u64,
    pub type_counts: Vec<(String, u64)>,
    pub language_counts: Vec<(String, u64)>,
    pub codec_counts: Vec<(String, u64)>,
    pub channel_layout_counts: Vec<(String, u64)>,
    pub sample_rate_counts: Vec<(String, u64)>,
    pub bit_depth_counts: Vec<(String, u64)>,
}

/// Aggregate subtitle track statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtitleStats {
    pub total_tracks: u64,
    pub language_counts: Vec<(String, u64)>,
    pub type_counts: Vec<(String, u64)>,
    pub external_count: u64,
}

/// Aggregate processing statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessingAggregateStats {
    pub plans_by_status: Vec<(String, u64)>,
    pub outcomes: Vec<(String, u64)>,
    pub total_processing_time_ms: u64,
    pub total_size_saved_bytes: i64,
    pub bad_file_count: u64,
    pub bad_files_by_source: Vec<(String, u64)>,
}

/// Aggregate job statistics.
#[non_exhaustive]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobAggregateStats {
    pub by_status: Vec<(String, u64)>,
    pub by_type: Vec<(String, u64)>,
}

impl LibrarySnapshot {
    #[must_use]
    pub fn new(
        trigger: SnapshotTrigger,
        files: FileStats,
        video: VideoStats,
        audio: AudioStats,
        subtitles: SubtitleStats,
        processing: ProcessingAggregateStats,
        jobs: JobAggregateStats,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            captured_at: Utc::now(),
            trigger,
            files,
            video,
            audio,
            subtitles,
            processing,
            jobs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_outcome_parse() {
        assert_eq!(
            ProcessingOutcome::parse("success"),
            Some(ProcessingOutcome::Success)
        );
        assert_eq!(
            ProcessingOutcome::parse("completed"),
            Some(ProcessingOutcome::Success)
        );
        assert_eq!(
            ProcessingOutcome::parse("failure"),
            Some(ProcessingOutcome::Failure)
        );
        assert_eq!(
            ProcessingOutcome::parse("failed"),
            Some(ProcessingOutcome::Failure)
        );
        assert_eq!(
            ProcessingOutcome::parse("skipped"),
            Some(ProcessingOutcome::Skipped)
        );
        assert_eq!(ProcessingOutcome::parse("unknown"), None);
    }

    #[test]
    fn test_processing_outcome_as_str() {
        assert_eq!(ProcessingOutcome::Success.as_str(), "success");
        assert_eq!(ProcessingOutcome::Failure.as_str(), "failure");
        assert_eq!(ProcessingOutcome::Skipped.as_str(), "skipped");
    }

    #[test]
    fn test_processing_outcome_display() {
        assert_eq!(format!("{}", ProcessingOutcome::Success), "success");
        assert_eq!(format!("{}", ProcessingOutcome::Failure), "failure");
    }

    #[test]
    fn test_snapshot_trigger_as_str() {
        assert_eq!(SnapshotTrigger::ScanComplete.as_str(), "scan_complete");
        assert_eq!(
            SnapshotTrigger::IntrospectComplete.as_str(),
            "introspect_complete"
        );
        assert_eq!(SnapshotTrigger::Manual.as_str(), "manual");
    }

    #[test]
    fn test_library_snapshot_serde_roundtrip() {
        let snapshot = LibrarySnapshot {
            id: Uuid::new_v4(),
            captured_at: Utc::now(),
            trigger: SnapshotTrigger::Manual,
            files: FileStats::default(),
            video: VideoStats::default(),
            audio: AudioStats::default(),
            subtitles: SubtitleStats::default(),
            processing: ProcessingAggregateStats::default(),
            jobs: JobAggregateStats::default(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: LibrarySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, snapshot.id);
        assert_eq!(deserialized.trigger, SnapshotTrigger::Manual);
        assert_eq!(deserialized.files.total_count, 0);
    }
}
