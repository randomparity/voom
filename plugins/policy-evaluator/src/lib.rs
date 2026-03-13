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
    #[must_use] 
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Evaluate],
            policies: Mutex::new(HashMap::new()),
        }
    }

    /// Register a compiled policy by name.
    pub fn register_policy(&self, policy: CompiledPolicy) -> Result<()> {
        let name = policy.name.clone();
        self.policies
            .lock()
            .map_err(|_| VoomError::Plugin {
                plugin: "policy-evaluator".into(),
                message: "policies lock poisoned".into(),
            })?
            .insert(name, policy);
        Ok(())
    }

    /// Evaluate a policy against a file, returning plans for all phases.
    pub fn evaluate(&self, policy_name: &str, file: &MediaFile) -> Result<Vec<Plan>> {
        let policies = self.policies.lock().map_err(|_| VoomError::Plugin {
            plugin: "policy-evaluator".into(),
            message: "policies lock poisoned".into(),
        })?;
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
            // NOTE: Policy evaluation is triggered via direct API call (evaluator::evaluate()),
            // not through the event bus. This handler is reserved for future event-driven
            // evaluation once storage integration is wired up.
            Event::PolicyEvaluate(evt) => {
                tracing::info!(
                    path = %evt.path.display(),
                    policy = %evt.policy_name,
                    "Evaluating policy"
                );

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::PolicyEvaluateEvent;

    #[test]
    fn new_creates_plugin_with_evaluate_capability() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert_eq!(plugin.capabilities(), &[Capability::Evaluate]);
    }

    #[test]
    fn default_creates_same_as_new() {
        let plugin = PolicyEvaluatorPlugin::default();
        assert_eq!(plugin.capabilities(), &[Capability::Evaluate]);
        assert_eq!(plugin.name(), "policy-evaluator");
    }

    #[test]
    fn plugin_name_and_version() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert_eq!(plugin.name(), "policy-evaluator");
        assert!(!plugin.version().is_empty());
    }

    #[test]
    fn handles_policy_evaluate_event_type() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert!(plugin.handles("policy.evaluate"));
        assert!(!plugin.handles("file.introspected"));
        assert!(!plugin.handles("plan.created"));
        assert!(!plugin.handles(""));
    }

    #[test]
    fn register_and_evaluate_unknown_policy_errors() {
        let plugin = PolicyEvaluatorPlugin::new();
        let file = MediaFile::new(PathBuf::from("/test/video.mkv"));
        let result = plugin.evaluate("nonexistent", &file);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unknown policy: nonexistent"));
    }

    #[test]
    fn on_event_returns_none_for_unhandled_event() {
        let plugin = PolicyEvaluatorPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.0".into(),
            path: PathBuf::from("/usr/bin/ffprobe"),
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn on_event_handles_policy_evaluate_event() {
        let plugin = PolicyEvaluatorPlugin::new();
        let event = Event::PolicyEvaluate(PolicyEvaluateEvent {
            path: PathBuf::from("/test/video.mkv"),
            policy_name: "test-policy".into(),
        });
        // Should return Ok(None) — logs a warning but doesn't fail
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn init_succeeds() {
        let mut plugin = PolicyEvaluatorPlugin::new();
        let ctx = PluginContext {
            config: serde_json::Value::Null,
            data_dir: PathBuf::from("/tmp/voom-test"),
        };
        assert!(plugin.init(&ctx).is_ok());
    }

    #[test]
    fn policies_mutex_starts_empty() {
        let plugin = PolicyEvaluatorPlugin::new();
        let policies = plugin.policies.lock().unwrap();
        assert!(policies.is_empty());
    }
}
