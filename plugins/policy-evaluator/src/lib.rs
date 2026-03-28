//! Policy Evaluator Plugin.
//!
//! Evaluates compiled policies against introspected media files to produce
//! [`Plan`](voom_domain::plan::Plan) structs describing the operations needed. Pure logic plugin with
//! no external dependencies.

#![allow(clippy::missing_errors_doc)]

pub mod condition;
pub mod evaluator;
pub mod filter;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::media::MediaFile;
use voom_dsl::compiled::CompiledPolicy;
use voom_kernel::{Plugin, PluginContext};

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

impl Plugin for PolicyEvaluatorPlugin {
    fn name(&self) -> &str {
        "policy-evaluator"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        // Policy evaluation is done via direct API call (evaluate()),
        // not through the event bus, so no capabilities are advertised.
        &[]
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::Event;

    #[test]
    fn test_new_creates_plugin_with_no_capabilities() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert!(plugin.capabilities().is_empty());
    }

    #[test]
    fn test_default_creates_same_as_new() {
        let plugin = PolicyEvaluatorPlugin;
        assert!(plugin.capabilities().is_empty());
        assert_eq!(plugin.name(), "policy-evaluator");
    }

    #[test]
    fn test_plugin_name_and_version() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert_eq!(plugin.name(), "policy-evaluator");
        assert!(!plugin.version().is_empty());
    }

    #[test]
    fn test_handles_no_events_since_evaluation_is_direct_api() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert!(!plugin.handles(Event::FILE_INTROSPECTED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
        assert!(!plugin.handles(""));
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

    #[test]
    fn test_on_event_returns_none_for_unhandled_event() {
        let plugin = PolicyEvaluatorPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.0",
            PathBuf::from("/usr/bin/ffprobe"),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_init_succeeds() {
        let mut plugin = PolicyEvaluatorPlugin::new();
        let ctx = PluginContext::new(serde_json::Value::Null, PathBuf::from("/tmp/voom-test"));
        assert!(plugin.init(&ctx).is_ok());
    }
}
