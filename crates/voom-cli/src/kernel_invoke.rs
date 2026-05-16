//! Typed helpers wrapping `Kernel::dispatch_to_capability` for the CLI's
//! runtime evaluator/orchestrator/scan call sites.
//!
//! Each helper builds the matching `Call`, dispatches via the kernel,
//! validates the `CallResponse` variant, and returns the unwrapped inner
//! result. Wrong response variants and dispatch errors are turned into
//! actionable `anyhow::Error`s so the CLI can fail closed with context.
//!
//! This module is purely additive: callers may keep using the free
//! `voom_policy_evaluator::*` functions for code paths that do not have a
//! kernel handle (e.g. `voom policy test`, `voom policy diff`, integration
//! tests of pure plan shapes).

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::{Capability, CapabilityQuery};
use voom_domain::capability_map::CapabilityMap;
use voom_domain::evaluation::{EvaluationOutcome, EvaluationResult};
use voom_domain::media::MediaFile;
use voom_domain::plan::PhaseOutput;
use voom_dsl::compiled::CompiledPolicy;
use voom_kernel::Kernel;

/// Dispatch a policy evaluation through the kernel's `Capability::EvaluatePolicy`
/// handler (the `PolicyEvaluatorPlugin` registered at bootstrap).
///
/// Records one row in `plugin_stats` per call (kernel-instrumented at the
/// Call boundary). Use this from CLI runtime paths so every evaluation is
/// host-measured.
///
/// `phase`: if `Some(name)`, the evaluator runs the single named phase;
/// `None` evaluates every phase in policy order.
///
/// `phase_outputs`: optional `HashMap<phase_name, PhaseOutput>` used by the
/// `evaluate_*_with_phase_outputs` codepath for cross-phase field lookups.
///
/// `phase_outcomes`: optional per-phase outcomes used by single-phase
/// evaluation to evaluate `skip_when` / `run_if` / `depends_on` conditions.
///
/// `capabilities_override`: if `Some`, used verbatim. If `None`, the plugin
/// consults its cached snapshot populated from `Event::ExecutorCapabilities`.
///
/// # Errors
/// - Capability not claimed (bootstrap bug): error names the missing capability.
/// - Plugin returned the wrong `CallResponse` variant: hard error naming the
///   variant returned. Cannot happen with the in-tree `PolicyEvaluatorPlugin`
///   but is checked because a future WASM plugin could violate this.
/// - Plugin's `on_call` returned `Err`: propagated.
/// - Plugin panicked: caught at the kernel boundary, returned as `Err`.
pub fn evaluate(
    kernel: &Kernel,
    policy: CompiledPolicy,
    file: MediaFile,
    phase: Option<String>,
    phase_outputs: Option<HashMap<String, PhaseOutput>>,
    phase_outcomes: Option<HashMap<String, EvaluationOutcome>>,
    capabilities_override: Option<CapabilityMap>,
) -> Result<EvaluationResult> {
    let call = Call::EvaluatePolicy {
        policy: Box::new(policy),
        file: Box::new(file),
        phase,
        phase_outputs,
        phase_outcomes,
        capabilities_override,
    };
    // String-typed kind, derived from `Capability::kind()`, so the canonical
    // wire token ("evaluate_policy") flows through the domain enum rather
    // than being duplicated as a literal.
    let query = CapabilityQuery::Exclusive {
        kind: Capability::EvaluatePolicy.kind().to_string(),
    };
    let response = kernel
        .dispatch_to_capability(query, call)
        .map_err(anyhow::Error::new)?;
    match response {
        CallResponse::EvaluatePolicy(result) => Ok(result),
        other => Err(anyhow!(
            "evaluate dispatch returned wrong CallResponse variant: {other:?}"
        )),
    }
}

/// Dispatch phase orchestration through the kernel's
/// `Capability::OrchestratePhases` handler (the `PhaseOrchestratorPlugin`
/// registered at bootstrap).
///
/// Records one row in `plugin_stats` per call (kernel-instrumented at the
/// Call boundary). Use this from CLI runtime paths so every orchestration is
/// host-measured.
///
/// `policy_name` is included in the Call payload per spec (useful for stats
/// attribution and future cross-cutting features); the current orchestrator
/// implementation doesn't read it.
///
/// # Errors
/// - Capability not claimed (bootstrap bug): error names the missing capability.
/// - Plugin returned the wrong `CallResponse` variant: hard error naming the
///   variant returned. Cannot happen with the in-tree `PhaseOrchestratorPlugin`
///   but is checked because a future WASM plugin could violate this.
/// - Plugin's `on_call` returned `Err`: propagated.
/// - Plugin panicked: caught at the kernel boundary, returned as `Err`.
pub fn orchestrate(
    kernel: &Kernel,
    plans: Vec<voom_domain::plan::Plan>,
    policy_name: String,
) -> Result<voom_phase_orchestrator::OrchestrationResult> {
    let call = Call::Orchestrate { plans, policy_name };
    let query = CapabilityQuery::Exclusive {
        kind: Capability::OrchestratePhases.kind().to_string(),
    };
    let response = kernel
        .dispatch_to_capability(query, call)
        .map_err(anyhow::Error::new)?;
    match response {
        CallResponse::Orchestrate(result) => Ok(result),
        other => Err(anyhow!(
            "orchestrate dispatch returned wrong CallResponse variant: {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use voom_policy_evaluator::PolicyEvaluatorPlugin;

    fn kernel_with_evaluator() -> Kernel {
        let mut kernel = Kernel::new();
        let ctx = voom_kernel::PluginContext::new(serde_json::json!({}), std::env::temp_dir());
        kernel
            .init_and_register(Arc::new(PolicyEvaluatorPlugin::for_bootstrap()), 36, &ctx)
            .expect("init_and_register policy-evaluator");
        kernel
    }

    #[test]
    fn evaluate_returns_one_plan_for_one_phase_policy() {
        let kernel = kernel_with_evaluator();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let result = evaluate(&kernel, policy, file, None, None, None, None)
            .expect("evaluate should succeed");
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].phase_name, "init");
    }

    #[test]
    fn evaluate_single_phase_present_returns_one_plan() {
        let kernel = kernel_with_evaluator();
        let policy = voom_dsl::compile_policy(
            r#"policy "demo" { phase a { container mkv } phase b { container mkv } }"#,
        )
        .unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let result = evaluate(
            &kernel,
            policy,
            file,
            Some("b".into()),
            None,
            Some(HashMap::new()),
            None,
        )
        .expect("evaluate should succeed");
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].phase_name, "b");
    }

    #[test]
    fn evaluate_single_phase_missing_returns_zero_plans() {
        let kernel = kernel_with_evaluator();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let result = evaluate(
            &kernel,
            policy,
            file,
            Some("does-not-exist".into()),
            None,
            Some(HashMap::new()),
            None,
        )
        .expect("evaluate should succeed even when phase is missing");
        assert!(result.plans.is_empty());
    }

    #[test]
    fn evaluate_without_evaluator_registered_is_hard_error() {
        let kernel = Kernel::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/movies/test.mkv"));

        let err = evaluate(&kernel, policy, file, None, None, None, None)
            .expect_err("dispatch without a handler must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("EvaluatePolicy")
                || chain.contains("evaluate_policy")
                || chain.contains("no handler"),
            "error must name the missing capability or 'no handler'; got: {chain}"
        );
    }

    fn kernel_with_orchestrator() -> voom_kernel::Kernel {
        let mut kernel = voom_kernel::Kernel::new();
        let ctx = voom_kernel::PluginContext::new(serde_json::json!({}), std::env::temp_dir());
        kernel
            .init_and_register(
                std::sync::Arc::new(
                    voom_phase_orchestrator::PhaseOrchestratorPlugin::for_bootstrap(),
                ),
                37,
                &ctx,
            )
            .expect("init_and_register phase-orchestrator");
        kernel
    }

    #[test]
    fn orchestrate_returns_result_for_single_phase_policy() {
        let kernel = kernel_with_orchestrator();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = voom_domain::media::MediaFile::new(std::path::PathBuf::from("/movies/test.mkv"));
        let plans = voom_policy_evaluator::evaluator::evaluate(&policy, &file).plans;

        let result =
            orchestrate(&kernel, plans, "demo".into()).expect("orchestrate should succeed");
        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].phase_name, "init");
    }

    #[test]
    fn orchestrate_with_empty_plans_returns_empty_result() {
        let kernel = kernel_with_orchestrator();
        let result =
            orchestrate(&kernel, vec![], "demo".into()).expect("orchestrate should succeed");
        assert!(result.plans.is_empty());
        assert!(result.phase_results.is_empty());
        assert!(!result.file_modified);
    }

    #[test]
    fn orchestrate_without_orchestrator_registered_is_hard_error() {
        let kernel = voom_kernel::Kernel::new();
        let err = orchestrate(&kernel, vec![], "demo".into())
            .expect_err("dispatch without a handler must fail");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("OrchestratePhases")
                || chain.contains("orchestrate_phases")
                || chain.contains("no handler"),
            "error must name the missing capability or 'no handler'; got: {chain}"
        );
    }
}
