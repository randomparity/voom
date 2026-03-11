use std::sync::{Arc, RwLock};
use voom_domain::events::{Event, EventResult};

use crate::Plugin;

struct Subscriber {
    plugin_name: String,
    priority: i32,
    handler: Arc<dyn Plugin>,
}

/// Event bus that dispatches events to subscribed plugins, ordered by priority.
pub struct EventBus {
    subscribers: RwLock<Vec<Subscriber>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: RwLock::new(Vec::new()),
        }
    }

    /// Subscribe a plugin to receive events it handles.
    /// Lower priority values run first.
    pub fn subscribe_plugin(&self, plugin: Arc<dyn Plugin>, priority: i32) {
        let mut subs = self.subscribers.write().expect("lock poisoned");
        let name = plugin.name().to_string();
        subs.push(Subscriber {
            plugin_name: name,
            priority,
            handler: plugin,
        });
        // Keep sorted by priority (lower first).
        subs.sort_by_key(|s| s.priority);
    }

    /// Publish an event to all subscribers that handle its type.
    /// Returns results from all handlers, in priority order.
    pub async fn publish(&self, event: Event) -> Vec<EventResult> {
        let event_type = event.event_type().to_string();

        // Collect matching handlers under the read lock, then release it.
        let handlers: Vec<(String, Arc<dyn Plugin>)> = {
            let subs = self.subscribers.read().expect("lock poisoned");
            subs.iter()
                .filter(|s| s.handler.handles(&event_type))
                .map(|s| (s.plugin_name.clone(), s.handler.clone()))
                .collect()
        };

        let mut results = Vec::new();
        for (name, handler) in handlers {
            match handler.on_event(&event) {
                Ok(Some(result)) => {
                    tracing::debug!(plugin = %name, event = %event_type, "event handled");
                    results.push(result);
                }
                Ok(None) => {
                    tracing::debug!(plugin = %name, event = %event_type, "event acknowledged (no result)");
                }
                Err(e) => {
                    tracing::error!(plugin = %name, event = %event_type, error = %e, "plugin error");
                }
            }
        }

        results
    }

    /// Returns the number of subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().expect("lock poisoned").len()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{FileDiscoveredEvent, ToolDetectedEvent};

    struct TestPlugin {
        name: String,
        handled_types: Vec<String>,
    }

    impl TestPlugin {
        fn new(name: &str, types: &[&str]) -> Self {
            Self {
                name: name.to_string(),
                handled_types: types.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, event_type: &str) -> bool {
            self.handled_types.iter().any(|t| t == event_type)
        }
        fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            Ok(Some(EventResult {
                plugin_name: self.name.clone(),
                produced_events: vec![],
                data: None,
            }))
        }
    }

    #[tokio::test]
    async fn test_publish_dispatches_to_matching_handlers() {
        let bus = EventBus::new();

        let p1 = Arc::new(TestPlugin::new("discovery", &["file.discovered"]));
        let p2 = Arc::new(TestPlugin::new("introspector", &["file.discovered"]));
        let p3 = Arc::new(TestPlugin::new("job-manager", &["job.started"]));

        bus.subscribe_plugin(p1, 0);
        bus.subscribe_plugin(p2, 10);
        bus.subscribe_plugin(p3, 0);

        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: "/test.mkv".into(),
            size: 1024,
            content_hash: "abc123".to_string(),
        });

        let results = bus.publish(event).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "discovery");
        assert_eq!(results[1].plugin_name, "introspector");
    }

    #[tokio::test]
    async fn test_publish_respects_priority_order() {
        let bus = EventBus::new();

        let p1 = Arc::new(TestPlugin::new("low-priority", &["tool.detected"]));
        let p2 = Arc::new(TestPlugin::new("high-priority", &["tool.detected"]));

        bus.subscribe_plugin(p1, 100);
        bus.subscribe_plugin(p2, 1);

        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.0".into(),
            path: "/usr/bin/ffprobe".into(),
        });

        let results = bus.publish(event).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "high-priority");
        assert_eq!(results[1].plugin_name, "low-priority");
    }

    #[tokio::test]
    async fn test_no_matching_handlers() {
        let bus = EventBus::new();
        let p = Arc::new(TestPlugin::new("discovery", &["file.discovered"]));
        bus.subscribe_plugin(p, 0);

        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.0".into(),
            path: "/usr/bin/ffprobe".into(),
        });

        let results = bus.publish(event).await;
        assert!(results.is_empty());
    }
}
