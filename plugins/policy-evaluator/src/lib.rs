//! Policy Evaluator Plugin.
//!
//! Evaluates compiled policies against introspected media files to produce
//! [`Plan`](voom_domain::plan::Plan) structs describing the operations needed. Pure logic plugin with
//! no external dependencies.

pub mod condition;
pub mod container_compat;
pub mod evaluator;
pub mod filter;

use std::collections::HashMap;

use voom_domain::capability_map::CapabilityMap;
use voom_domain::media::MediaFile;
use voom_dsl::compiled::CompiledPolicy;

pub use evaluator::{
    EvaluationOutcome, apply_capability_hints, evaluate, evaluate_with_context,
    evaluate_with_phase_outputs,
};

/// Evaluate a policy with system capabilities available to conditions,
/// then validate plans against executor capabilities.
///
/// This is a convenience wrapper that calls [`evaluator::evaluate_with_context`]
/// followed by [`evaluator::apply_capability_hints`]. The plain [`evaluate`]
/// function remains available for callers that don't need capability context.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// use voom_domain::capability_map::CapabilityMap;
/// use voom_domain::media::MediaFile;
/// use voom_dsl::compile_policy;
/// use voom_policy_evaluator::evaluate_with_capabilities;
///
/// let policy = compile_policy(r#"policy "demo" {
///     phase init {
///         container mkv
///     }
/// }"#).unwrap();
///
/// let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));
/// let caps = CapabilityMap::default();
/// let result = evaluate_with_capabilities(&policy, &file, &caps);
/// assert_eq!(result.plans.len(), 1);
/// assert_eq!(result.plans[0].phase_name, "init");
/// ```
#[must_use]
pub fn evaluate_with_capabilities(
    policy: &CompiledPolicy,
    file: &MediaFile,
    capabilities: &CapabilityMap,
) -> evaluator::EvaluationResult {
    let mut result = evaluator::evaluate_with_context(policy, file, Some(capabilities));
    evaluator::apply_capability_hints(&mut result.plans, capabilities);
    result
}

/// Evaluate a single phase against the current file state.
///
/// Used by the per-phase evaluate-execute-reintrospect loop so each
/// phase sees the file as it exists after prior phases have executed.
#[must_use]
pub fn evaluate_single_phase_with_hints(
    phase_name: &str,
    policy: &CompiledPolicy,
    file: &MediaFile,
    phase_outcomes: &HashMap<String, EvaluationOutcome>,
    capabilities: &CapabilityMap,
) -> Option<voom_domain::plan::Plan> {
    let mut plan = evaluator::evaluate_single_phase(
        phase_name,
        policy,
        file,
        phase_outcomes,
        Some(capabilities),
    )?;
    evaluator::apply_capability_hints(std::slice::from_mut(&mut plan), capabilities);
    Some(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_evaluate_returns_result_with_plans() {
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/test/video.mkv"));
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
    }
}
