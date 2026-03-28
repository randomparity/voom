//! Policy Evaluator Plugin.
//!
//! Evaluates compiled policies against introspected media files to produce
//! [`Plan`](voom_domain::plan::Plan) structs describing the operations needed. Pure logic plugin with
//! no external dependencies.

pub mod condition;
pub mod evaluator;
pub mod filter;

use voom_domain::media::MediaFile;
use voom_dsl::compiled::CompiledPolicy;

/// The policy evaluator plugin.
///
/// Evaluates compiled policies against media files and produces `Plan` structs.
/// Evaluation is done via direct API call, not through the event bus.
pub struct PolicyEvaluatorPlugin;

impl PolicyEvaluatorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Evaluate a compiled policy against a media file, producing an [`evaluator::EvaluationResult`]
    /// with plans for all phases and per-phase outcomes.
    pub fn evaluate(
        &self,
        policy: &CompiledPolicy,
        file: &MediaFile,
    ) -> evaluator::EvaluationResult {
        evaluator::evaluate(policy, file)
    }
}

impl Default for PolicyEvaluatorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_default_creates_same_as_new() {
        let _plugin = PolicyEvaluatorPlugin;
    }

    #[test]
    fn test_evaluate_returns_result_with_plans() {
        let plugin = PolicyEvaluatorPlugin::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/test/video.mkv"));
        let result = plugin.evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
    }
}
