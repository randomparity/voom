//! Policy evaluator plugin: claims `Capability::EvaluatePolicy` and routes
//! `Call::EvaluatePolicy` to the existing free-function evaluator surface.
//!
//! The plugin subscribes to `Event::EXECUTOR_CAPABILITIES` to keep a cached
//! `CapabilityMap`, which `on_call` consults when no `capabilities_override`
//! rides with the call. This mirrors today's capability-collector pattern
//! while exposing a Call-handling capability the kernel can route to.

use parking_lot::RwLock;

use voom_domain::capabilities::Capability;
use voom_domain::capability_map::CapabilityMap;
use voom_domain::events::{Event, EventResult};
use voom_kernel::Plugin;

/// Policy evaluator plugin — claims `Capability::EvaluatePolicy` (Exclusive)
/// and maintains a cached `CapabilityMap` from `ExecutorCapabilities` events.
pub struct PolicyEvaluatorPlugin {
    capabilities: Vec<Capability>,
    cache: RwLock<CapabilityMap>,
}

impl PolicyEvaluatorPlugin {
    /// Bootstrap-only constructor — the canonical public entry point.
    ///
    /// All runtime evaluator invocations flow through
    /// `Kernel::dispatch_to_capability(Exclusive(EvaluatePolicy), Call::EvaluatePolicy)`.
    /// Constructing a `PolicyEvaluatorPlugin` directly is legitimate only at
    /// kernel-bootstrap registration time and inside in-crate tests
    /// (via the `pub(crate) fn new()` companion).
    #[must_use]
    pub fn for_bootstrap() -> Self {
        Self::new()
    }

    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            capabilities: vec![Capability::EvaluatePolicy],
            cache: RwLock::new(CapabilityMap::new()),
        }
    }
}

impl Plugin for PolicyEvaluatorPlugin {
    fn name(&self) -> &'static str {
        "policy-evaluator"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::EXECUTOR_CAPABILITIES
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::ExecutorCapabilities(caps) = event {
            self.cache.write().register(caps.clone());
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};

    #[test]
    fn claims_exclusive_evaluate_policy_capability() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert_eq!(plugin.capabilities(), &[Capability::EvaluatePolicy]);
        assert_eq!(
            Capability::EvaluatePolicy.resolution(),
            voom_domain::capability_resolution::CapabilityResolution::Exclusive,
            "EvaluatePolicy must be Exclusive — second plugin claiming it fails at register"
        );
    }

    #[test]
    fn subscribes_only_to_executor_capabilities_events() {
        let plugin = PolicyEvaluatorPlugin::new();
        assert!(plugin.handles(Event::EXECUTOR_CAPABILITIES));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn on_event_populates_capability_cache() {
        let plugin = PolicyEvaluatorPlugin::new();
        let event = Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(vec!["h264".into()], vec!["libx264".into(), "aac".into()]),
            vec!["matroska".into()],
            vec![],
        ));

        plugin.on_event(&event).expect("on_event should not fail");

        let snapshot = plugin.cache.read().clone();
        assert!(!snapshot.is_empty(), "cache must be populated after event");
        assert!(snapshot.has_encoder("libx264"));
        assert!(snapshot.has_format("matroska"));
    }
}
