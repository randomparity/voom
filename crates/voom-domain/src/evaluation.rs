//! Shared types describing policy-evaluation outcomes and results.

use crate::plan::Plan;
use serde::{Deserialize, Serialize};

/// Outcome of evaluating a single phase.
///
/// Used during full-policy evaluation to thread per-phase state to subsequent
/// phases (e.g. `phase_outcomes` parameter to `evaluate_single_phase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvaluationOutcome {
    Executed { modified: bool },
    Skipped,
    SafeguardFailed,
    ExecutionFailed,
}

/// Result of evaluating a compiled policy: the plans produced for each phase,
/// in policy `phase_order` order.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    pub plans: Vec<Plan>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_outcome_roundtrips() {
        let cases = [
            EvaluationOutcome::Executed { modified: true },
            EvaluationOutcome::Executed { modified: false },
            EvaluationOutcome::Skipped,
            EvaluationOutcome::SafeguardFailed,
            EvaluationOutcome::ExecutionFailed,
        ];
        for o in cases {
            let json = serde_json::to_string(&o).expect("ser");
            let back: EvaluationOutcome = serde_json::from_str(&json).expect("de");
            assert_eq!(o, back);
        }
    }

    #[test]
    fn evaluation_result_roundtrips() {
        let r = EvaluationResult { plans: vec![] };
        let json = serde_json::to_string(&r).expect("ser");
        let back: EvaluationResult = serde_json::from_str(&json).expect("de");
        assert_eq!(back.plans.len(), 0);
    }
}
