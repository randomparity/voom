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
#[non_exhaustive]
pub struct PhaseOrchestratorPlugin {
    capabilities: Vec<Capability>,
}

impl PhaseOrchestratorPlugin {
    /// Bootstrap-only constructor.
    #[must_use]
    pub fn for_bootstrap() -> Self {
        Self {
            capabilities: vec![Capability::OrchestratePhases],
        }
    }
}

impl Plugin for PhaseOrchestratorPlugin {
    fn name(&self) -> &str {
        "phase-orchestrator"
    }
    fn version(&self) -> &str {
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
                "phase-orchestrator",
                format!("phase-orchestrator only handles Call::Orchestrate, got {call:?}"),
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
