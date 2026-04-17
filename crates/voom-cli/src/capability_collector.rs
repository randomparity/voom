use parking_lot::Mutex;

use voom_domain::capabilities::Capability;
use voom_domain::capability_map::CapabilityMap;
use voom_domain::events::{Event, EventResult};

/// Lightweight internal plugin that collects `ExecutorCapabilities` events
/// emitted during plugin init, making the aggregated data available via
/// [`snapshot`](Self::snapshot) for the policy evaluator.
pub struct CapabilityCollectorPlugin {
    map: Mutex<CapabilityMap>,
}

impl CapabilityCollectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: Mutex::new(CapabilityMap::new()),
        }
    }

    /// Returns a clone of the collected capability map.
    #[must_use]
    pub fn snapshot(&self) -> CapabilityMap {
        self.map.lock().clone()
    }
}

impl voom_kernel::Plugin for CapabilityCollectorPlugin {
    fn name(&self) -> &'static str {
        "capability-collector"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn description(&self) -> &'static str {
        "Collects executor capability announcements for policy evaluation"
    }

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::EXECUTOR_CAPABILITIES
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::ExecutorCapabilities(ref caps) = event {
            self.map.lock().register(caps.clone());
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::{CodecCapabilities, ExecutorCapabilitiesEvent};
    use voom_kernel::Plugin;

    #[test]
    fn test_handles_executor_capabilities_events() {
        let plugin = CapabilityCollectorPlugin::new();
        assert!(plugin.handles(Event::EXECUTOR_CAPABILITIES));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn test_snapshot_returns_registered_capabilities() {
        let plugin = CapabilityCollectorPlugin::new();

        let event = Event::ExecutorCapabilities(ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            CodecCapabilities::new(vec!["h264".into()], vec!["libx264".into(), "aac".into()]),
            vec!["matroska".into()],
            vec![],
        ));

        plugin.on_event(&event).expect("on_event should not fail");

        let map = plugin.snapshot();
        assert!(!map.is_empty());
        assert!(map.has_encoder("libx264"));
        assert!(map.has_encoder("aac"));
        assert!(map.has_format("matroska"));
        assert!(!map.has_encoder("opus"));
    }

    #[test]
    fn test_empty_snapshot_before_any_events() {
        let plugin = CapabilityCollectorPlugin::new();
        let map = plugin.snapshot();
        assert!(map.is_empty());
    }
}
