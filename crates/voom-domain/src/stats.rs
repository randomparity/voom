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

/// Statistics recorded after processing a file through a policy phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingStats {
    pub id: Uuid,
    pub file_id: Uuid,
    pub policy_name: String,
    pub phase_name: String,
    pub outcome: ProcessingOutcome,
    pub duration_ms: u64,
    pub actions_taken: u32,
    pub tracks_modified: u32,
    pub file_size_before: Option<u64>,
    pub file_size_after: Option<u64>,
    pub created_at: DateTime<Utc>,
}

impl ProcessingStats {
    #[must_use]
    pub fn new(file_id: Uuid, policy_name: String, phase_name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            file_id,
            policy_name,
            phase_name,
            outcome: ProcessingOutcome::Success,
            duration_ms: 0,
            actions_taken: 0,
            tracks_modified: 0,
            file_size_before: None,
            file_size_after: None,
            created_at: Utc::now(),
        }
    }

    /// Returns the change in file size, if both before and after sizes are known.
    #[must_use]
    pub fn size_delta(&self) -> Option<i64> {
        match (self.file_size_before, self.file_size_after) {
            (Some(before), Some(after)) => {
                let a = i64::try_from(after).unwrap_or(i64::MAX);
                let b = i64::try_from(before).unwrap_or(i64::MAX);
                Some(a.saturating_sub(b))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_processing_stats_new() {
        let file_id = Uuid::new_v4();
        let stats = ProcessingStats::new(file_id, "default".into(), "normalize".into());
        assert_eq!(stats.file_id, file_id);
        assert_eq!(stats.policy_name, "default");
        assert_eq!(stats.duration_ms, 0);
        assert_eq!(stats.outcome, ProcessingOutcome::Success);
    }

    #[test]
    fn test_size_delta() {
        let mut stats = ProcessingStats::new(Uuid::new_v4(), "p".into(), "ph".into());
        assert_eq!(stats.size_delta(), None);
        stats.file_size_before = Some(1000);
        stats.file_size_after = Some(800);
        assert_eq!(stats.size_delta(), Some(-200));
    }

    #[test]
    fn test_stats_serde_roundtrip() {
        let stats = ProcessingStats::new(Uuid::new_v4(), "default".into(), "normalize".into());
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: ProcessingStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.policy_name, "default");
        assert_eq!(deserialized.outcome, ProcessingOutcome::Success);
    }

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
}
