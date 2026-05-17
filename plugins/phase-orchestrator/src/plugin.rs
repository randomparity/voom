//! Kernel-registered plugin wrapper for the phase orchestrator.
//!
//! The free functions in `lib.rs` (`orchestrate`, `format_dry_run`,
//! `needs_execution`, `phase_error_strategy`) remain the canonical library
//! API — they stay `pub`. `PhaseOrchestratorPlugin` is an *additional* entry
//! path that exposes the orchestrator through the kernel's
//! `dispatch_to_capability(Capability::OrchestratePhases, Call::Orchestrate)`
//! route, which is how the CLI invokes orchestration after Phase 5.
//!
//! The plugin is stateless: it subscribes to no events and has no internal
//! cache. Every `on_call` invocation delegates directly to the
//! `orchestrate(plans)` free function.

use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_kernel::Plugin;

/// Kernel-registered plugin that exposes `voom-phase-orchestrator` via Call dispatch.
pub struct PhaseOrchestratorPlugin {
    capabilities: Vec<Capability>,
}

impl PhaseOrchestratorPlugin {
    /// Bootstrap-only constructor — the canonical public entry point.
    ///
    /// All runtime orchestrator invocations flow through
    /// `Kernel::dispatch_to_capability(Exclusive(OrchestratePhases), Call::Orchestrate)`.
    /// Constructing a `PhaseOrchestratorPlugin` directly is legitimate only at
    /// kernel-bootstrap registration time and inside in-crate tests
    /// (via the `pub(crate) fn new()` companion).
    #[must_use]
    pub fn for_bootstrap() -> Self {
        Self::new()
    }

    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            capabilities: vec![Capability::OrchestratePhases],
        }
    }
}

impl Plugin for PhaseOrchestratorPlugin {
    fn name(&self) -> &'static str {
        "phase-orchestrator"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, _event_type: &str) -> bool {
        // PhaseOrchestratorPlugin is stateless; it does not subscribe to any
        // events. Its only entry point is on_call(Call::Orchestrate).
        false
    }

    fn on_call(&self, call: &Call) -> Result<CallResponse> {
        let Call::Orchestrate {
            plans,
            policy_name: _,
        } = call
        else {
            return Err(VoomError::plugin(
                self.name(),
                format!(
                    "PhaseOrchestratorPlugin only handles Call::Orchestrate, got {:?}",
                    std::mem::discriminant(call)
                ),
            ));
        };

        // Delegate to the existing pub free function. The plugin is purely a
        // dispatch surface; the orchestration logic remains in lib.rs.
        // `policy_name` is accepted as part of the Call payload (per spec)
        // but not currently read — it's pass-through metadata for stats
        // attribution and potential future cross-cutting use.
        let result = crate::orchestrate(plans.clone());
        Ok(CallResponse::Orchestrate(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::PhaseOutcome;

    fn test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/Movie.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![{
            let mut t = Track::new(0, TrackType::Video, "hevc".into());
            t.width = Some(1920);
            t.height = Some(1080);
            t
        }];
        file
    }

    fn eval(policy: &voom_dsl::CompiledPolicy, file: &MediaFile) -> Vec<voom_domain::plan::Plan> {
        voom_policy_evaluator::evaluator::evaluate(policy, file).plans
    }

    #[test]
    fn on_call_orchestrate_delegates_to_free_function() {
        let plugin = PhaseOrchestratorPlugin::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let plans = eval(&policy, &file);

        let direct = crate::orchestrate(plans.clone());

        let call = Call::Orchestrate {
            plans: plans.clone(),
            policy_name: "demo".into(),
        };
        let response = plugin.on_call(&call).expect("on_call ok");

        match response {
            CallResponse::Orchestrate(routed) => {
                // Full-struct equality via serde — OrchestrationResult is
                // Serialize per Phase 1, so this comparison catches every
                // field-level divergence including phase_results, outcomes,
                // skip reasons, and file_modified.
                let direct_v = serde_json::to_value(&direct).expect("serialize direct");
                let routed_v = serde_json::to_value(&routed).expect("serialize routed");
                assert_eq!(
                    direct_v, routed_v,
                    "kernel-routed OrchestrationResult differs from direct call"
                );
            }
            _ => panic!("wrong CallResponse variant"),
        }
    }

    #[test]
    fn on_call_preserves_phase_outcomes() {
        // Multi-phase policy where outcomes reliably reflect orchestrate() logic
        // (skip vs. pending vs. completed). Confirm the routed plugin path
        // produces identical outcomes.
        let plugin = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "demo" {
                phase first { container mkv }
                phase second {
                    depends_on: [first]
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
            }"#,
        )
        .unwrap();
        let file = test_file(); // hevc — second phase should be skipped
        let plans = eval(&policy, &file);

        let call = Call::Orchestrate {
            plans,
            policy_name: "demo".into(),
        };
        let response = plugin.on_call(&call).expect("on_call ok");
        let CallResponse::Orchestrate(result) = response else {
            panic!("wrong variant");
        };

        assert_eq!(result.phase_results.len(), 2);
        assert_eq!(
            result.phase_results[1].outcome,
            PhaseOutcome::Skipped,
            "second phase should be Skipped (video already hevc)"
        );
    }

    #[test]
    fn on_call_rejects_non_orchestrate_call_variants() {
        let plugin = PhaseOrchestratorPlugin::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "demo" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let call = Call::EvaluatePolicy {
            policy: Box::new(policy),
            file: Box::new(file),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let err = plugin.on_call(&call).unwrap_err();
        assert!(
            err.to_string().contains("only handles Call::Orchestrate"),
            "error message should name the expected variant; got: {err}"
        );
    }

    #[test]
    fn handles_returns_false_for_all_events() {
        let plugin = PhaseOrchestratorPlugin::new();
        assert!(!plugin.handles(voom_domain::events::Event::FILE_DISCOVERED));
        assert!(!plugin.handles(voom_domain::events::Event::FILE_INTROSPECTED));
        assert!(!plugin.handles(voom_domain::events::Event::EXECUTOR_CAPABILITIES));
        assert!(!plugin.handles(voom_domain::events::Event::PLAN_CREATED));
        assert!(!plugin.handles("any-arbitrary-string"));
    }

    #[test]
    fn on_call_with_empty_plans_is_well_defined() {
        let plugin = PhaseOrchestratorPlugin::new();
        let call = Call::Orchestrate {
            plans: vec![],
            policy_name: "empty".into(),
        };
        let response = plugin.on_call(&call).expect("on_call ok");
        let CallResponse::Orchestrate(result) = response else {
            panic!("wrong variant");
        };
        assert!(result.plans.is_empty());
        assert!(result.phase_results.is_empty());
        assert!(!result.file_modified);
    }
}
