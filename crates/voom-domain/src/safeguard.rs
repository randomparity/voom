//! Processing safeguard types.
//!
//! Safeguards prevent destructive operations like stripping all audio or
//! video tracks from a file. When a safeguard triggers, the planned
//! actions are retracted and a [`SafeguardViolation`] is recorded on the
//! plan so it can be persisted, reported, and queried later.

use serde::{Deserialize, Serialize};

/// The category of safeguard violation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeguardKind {
    /// A keep/remove operation would have stripped all tracks of a type.
    AllTracksRemoved,
    /// Across all operations, no video track would survive.
    NoVideoTrack,
    /// Across all operations, no audio track would survive.
    NoAudioTrack,
    /// Output file was larger than the input.
    OutputLarger,
    /// Insufficient free disk space to safely execute the plan.
    DiskSpaceLow,
}

impl SafeguardKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AllTracksRemoved => "all_tracks_removed",
            Self::NoVideoTrack => "no_video_track",
            Self::NoAudioTrack => "no_audio_track",
            Self::OutputLarger => "output_larger",
            Self::DiskSpaceLow => "disk_space_low",
        }
    }
}

impl std::fmt::Display for SafeguardKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A safeguard violation detected during evaluation or execution.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeguardViolation {
    pub kind: SafeguardKind,
    pub message: String,
    pub phase_name: String,
}

impl SafeguardViolation {
    #[must_use]
    pub fn new(
        kind: SafeguardKind,
        message: impl Into<String>,
        phase_name: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            phase_name: phase_name.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safeguard_kind_display() {
        assert_eq!(
            SafeguardKind::AllTracksRemoved.to_string(),
            "all_tracks_removed"
        );
        assert_eq!(SafeguardKind::NoVideoTrack.to_string(), "no_video_track");
        assert_eq!(SafeguardKind::NoAudioTrack.to_string(), "no_audio_track");
        assert_eq!(SafeguardKind::OutputLarger.to_string(), "output_larger");
    }

    #[test]
    fn test_safeguard_violation_new() {
        let v = SafeguardViolation::new(
            SafeguardKind::AllTracksRemoved,
            "all audio removed",
            "track-selection",
        );
        assert_eq!(v.kind, SafeguardKind::AllTracksRemoved);
        assert_eq!(v.message, "all audio removed");
        assert_eq!(v.phase_name, "track-selection");
    }

    #[test]
    fn test_serde_roundtrip() {
        let v = SafeguardViolation::new(SafeguardKind::NoAudioTrack, "test message", "normalize");
        let json = serde_json::to_string(&v).expect("serialize");
        let deserialized: SafeguardViolation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, v);
    }

    #[test]
    fn test_safeguard_kind_disk_space_low_display() {
        assert_eq!(SafeguardKind::DiskSpaceLow.to_string(), "disk_space_low");
    }

    #[test]
    fn test_disk_space_low_serde_roundtrip() {
        let v = SafeguardViolation::new(
            SafeguardKind::DiskSpaceLow,
            "need 2.1 GB, only 500 MB available",
            "normalize",
        );
        let json = serde_json::to_string(&v).expect("serialize");
        let deserialized: SafeguardViolation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized, v);
    }
}
