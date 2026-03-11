use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Statistics recorded after processing a file through a policy phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingStats {
    pub id: Uuid,
    pub file_id: Uuid,
    pub policy_name: String,
    pub phase_name: String,
    pub outcome: String,
    pub duration_ms: u64,
    pub actions_taken: u32,
    pub tracks_modified: u32,
    pub file_size_before: Option<u64>,
    pub file_size_after: Option<u64>,
    pub created_at: DateTime<Utc>,
}

impl ProcessingStats {
    pub fn new(file_id: Uuid, policy_name: String, phase_name: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            file_id,
            policy_name,
            phase_name,
            outcome: String::new(),
            duration_ms: 0,
            actions_taken: 0,
            tracks_modified: 0,
            file_size_before: None,
            file_size_after: None,
            created_at: Utc::now(),
        }
    }

    /// Returns the change in file size, if both before and after sizes are known.
    pub fn size_delta(&self) -> Option<i64> {
        match (self.file_size_before, self.file_size_after) {
            (Some(before), Some(after)) => Some(after as i64 - before as i64),
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
    }
}
