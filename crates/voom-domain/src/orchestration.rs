//! Shared types describing phase-orchestration results.

use crate::plan::{PhaseResult, Plan};
use serde::{Deserialize, Serialize};

/// Result of orchestrating all phases of a policy.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationResult {
    /// Plans produced for each phase (in execution order).
    pub plans: Vec<Plan>,
    /// Results for each executed phase.
    pub phase_results: Vec<PhaseResult>,
    /// Whether any phase modified the file.
    pub file_modified: bool,
}

impl OrchestrationResult {
    #[must_use]
    pub fn new(plans: Vec<Plan>, phase_results: Vec<PhaseResult>, file_modified: bool) -> Self {
        Self {
            plans,
            phase_results,
            file_modified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestration_result_roundtrips() {
        let r = OrchestrationResult::new(vec![], vec![], false);
        let json = serde_json::to_string(&r).expect("ser");
        let back: OrchestrationResult = serde_json::from_str(&json).expect("de");
        assert_eq!(back.plans.len(), 0);
        assert_eq!(back.phase_results.len(), 0);
        assert!(!back.file_modified);
    }
}
