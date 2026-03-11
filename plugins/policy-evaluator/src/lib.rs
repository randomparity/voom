//! Policy Evaluator Plugin.
//!
//! Evaluates compiled policies against introspected media files to produce
//! [`Plan`] structs describing the operations needed. Pure logic plugin with
//! no external dependencies.

pub mod condition;
pub mod evaluator;
pub mod filter;

use std::collections::HashMap;
use std::sync::Mutex;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult};
use voom_domain::media::MediaFile;
use voom_domain::plan::Plan;
use voom_dsl::compiler::CompiledPolicy;
use voom_kernel::{Plugin, PluginContext};

/// The policy evaluator plugin.
///
/// Handles `policy.evaluate` events by evaluating a compiled policy against
/// a media file and emitting `plan.created` events for each phase.
pub struct PolicyEvaluatorPlugin {
    capabilities: Vec<Capability>,
    policies: Mutex<HashMap<String, CompiledPolicy>>,
}

impl PolicyEvaluatorPlugin {
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Evaluate],
            policies: Mutex::new(HashMap::new()),
        }
    }

    /// Register a compiled policy by name.
    pub fn register_policy(&self, policy: CompiledPolicy) {
        let name = policy.name.clone();
        self.policies.lock().unwrap().insert(name, policy);
    }

    /// Evaluate a policy against a file, returning plans for all phases.
    pub fn evaluate(&self, policy_name: &str, file: &MediaFile) -> Result<Vec<Plan>> {
        let policies = self.policies.lock().unwrap();
        let policy = policies.get(policy_name).ok_or_else(|| VoomError::Plugin {
            plugin: "policy-evaluator".into(),
            message: format!("Unknown policy: {policy_name}"),
        })?;

        let result = evaluator::evaluate(policy, file);
        Ok(result.plans)
    }

    /// Evaluate a policy directly (without registering it first).
    pub fn evaluate_policy(
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

impl Plugin for PolicyEvaluatorPlugin {
    fn name(&self) -> &str {
        "policy-evaluator"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "policy.evaluate"
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PolicyEvaluate(evt) => {
                tracing::info!(
                    path = %evt.path.display(),
                    policy = %evt.policy_name,
                    "Evaluating policy"
                );

                // In a full system, we'd look up the file from storage.
                // For now, we just log that we received the event.
                // The actual evaluation happens through the evaluate() method
                // when the orchestrator calls us with a concrete MediaFile.
                tracing::warn!(
                    "PolicyEvaluate event received but file lookup requires storage integration. \
                     Use evaluate() or evaluate_policy() directly."
                );

                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        tracing::info!("Policy evaluator plugin initialized");
        Ok(())
    }
}
