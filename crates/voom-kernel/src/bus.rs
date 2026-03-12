use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use parking_lot::RwLock;
use voom_domain::events::{Event, EventResult, PluginErrorEvent};

use crate::Plugin;

/// Maximum recursion depth for event cascading to prevent infinite loops.
const MAX_CASCADE_DEPTH: u8 = 8;

struct Subscriber {
    plugin_name: String,
    priority: i32,
    handler: Arc<dyn Plugin>,
}

/// Event bus that dispatches events to subscribed plugins, ordered by priority.
///
/// Dispatch is sequential: handlers run one at a time in priority order (lower
/// values first). There is no backpressure — every published event is delivered
/// to all matching subscribers immediately. This is intentional: the kernel is
/// the single orchestrator and predictable ordering simplifies reasoning about
/// plugin interactions.
///
/// Events produced by handlers are automatically cascaded (re-published) up to
/// a fixed depth limit to prevent infinite loops.
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
        let mut subs = self.subscribers.write();
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
    /// Produced events are automatically cascaded up to a depth limit.
    #[tracing::instrument(skip(self, event), fields(event_type = %event.event_type()))]
    pub fn publish(&self, event: Event) -> Vec<EventResult> {
        self.publish_recursive(event, 0)
    }

    /// Internal recursive dispatch. Dispatches the event to matching handlers,
    /// then cascades any produced events up to `MAX_CASCADE_DEPTH`.
    fn publish_recursive(&self, event: Event, depth: u8) -> Vec<EventResult> {
        let event_type = event.event_type().to_string();

        // Collect matching handlers under the read lock, then release it.
        let handlers: Vec<(String, Arc<dyn Plugin>)> = {
            let subs = self.subscribers.read();
            subs.iter()
                .filter(|s| s.handler.handles(&event_type))
                .map(|s| (s.plugin_name.clone(), s.handler.clone()))
                .collect()
        };

        let mut results = Vec::new();
        for (name, handler) in handlers {
            let handler_result = catch_unwind(AssertUnwindSafe(|| handler.on_event(&event)));

            match handler_result {
                Ok(Ok(Some(result))) => {
                    tracing::debug!(plugin = %name, event = %event_type, "event handled");
                    results.push(result);
                }
                Ok(Ok(None)) => {
                    tracing::debug!(plugin = %name, event = %event_type, "event acknowledged (no result)");
                }
                Ok(Err(e)) => {
                    tracing::error!(plugin = %name, event = %event_type, error = %e, "plugin error");
                    results.push(EventResult {
                        plugin_name: name.clone(),
                        produced_events: vec![Event::PluginError(PluginErrorEvent {
                            plugin_name: name.clone(),
                            event_type: event_type.clone(),
                            error: e.to_string(),
                        })],
                        data: None,
                    });
                }
                Err(_) => {
                    tracing::error!(plugin = %name, event = %event_type, "plugin panicked during event dispatch");
                    results.push(EventResult {
                        plugin_name: name.clone(),
                        produced_events: vec![Event::PluginError(PluginErrorEvent {
                            plugin_name: name.clone(),
                            event_type: event_type.clone(),
                            error: "plugin panicked".into(),
                        })],
                        data: None,
                    });
                }
            }
        }

        // Cascade produced events from all results.
        if depth < MAX_CASCADE_DEPTH {
            let produced: Vec<Event> = results
                .iter()
                .flat_map(|r| r.produced_events.clone())
                .collect();
            for cascaded_event in produced {
                let cascaded_results = self.publish_recursive(cascaded_event, depth + 1);
                results.extend(cascaded_results);
            }
        } else {
            let has_produced = results.iter().any(|r| !r.produced_events.is_empty());
            if has_produced {
                tracing::warn!(
                    depth = depth,
                    "event cascade depth limit reached, stopping recursion"
                );
            }
        }

        results
    }

    /// Returns the number of subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().len()
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
    use voom_domain::events::{FileDiscoveredEvent, FileIntrospectedEvent, ToolDetectedEvent};
    use voom_domain::media::MediaFile;

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

    #[test]
    fn test_publish_dispatches_to_matching_handlers() {
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

        let results = bus.publish(event);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "discovery");
        assert_eq!(results[1].plugin_name, "introspector");
    }

    #[test]
    fn test_publish_respects_priority_order() {
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

        let results = bus.publish(event);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "high-priority");
        assert_eq!(results[1].plugin_name, "low-priority");
    }

    #[test]
    fn test_no_matching_handlers() {
        let bus = EventBus::new();
        let p = Arc::new(TestPlugin::new("discovery", &["file.discovered"]));
        bus.subscribe_plugin(p, 0);

        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.0".into(),
            path: "/usr/bin/ffprobe".into(),
        });

        let results = bus.publish(event);
        assert!(results.is_empty());
    }

    // --- Error-returning plugin tests ---

    struct ErrorPlugin {
        name: String,
        handled_types: Vec<String>,
    }

    impl Plugin for ErrorPlugin {
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
            Err(voom_domain::errors::VoomError::Plugin {
                plugin: self.name.clone(),
                message: "something broke".into(),
            })
        }
    }

    #[test]
    fn test_error_returning_plugin_produces_plugin_error_event() {
        let bus = EventBus::new();

        let error_plugin = Arc::new(ErrorPlugin {
            name: "error-plugin".into(),
            handled_types: vec!["file.discovered".into()],
        });
        let normal = Arc::new(TestPlugin::new("good-plugin", &["file.discovered"]));

        bus.subscribe_plugin(error_plugin, 0);
        bus.subscribe_plugin(normal, 10);

        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: "/test.mkv".into(),
            size: 1024,
            content_hash: "abc123".to_string(),
        });

        let results = bus.publish(event);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "error-plugin");
        assert_eq!(results[0].produced_events.len(), 1);
        assert_eq!(results[0].produced_events[0].event_type(), "plugin.error");
        assert_eq!(results[1].plugin_name, "good-plugin");
    }

    // --- Panic handler tests ---

    struct PanickingPlugin {
        name: String,
        handled_types: Vec<String>,
    }

    impl PanickingPlugin {
        fn new(name: &str, types: &[&str]) -> Self {
            Self {
                name: name.to_string(),
                handled_types: types.iter().map(|s| s.to_string()).collect(),
            }
        }
    }

    impl Plugin for PanickingPlugin {
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
            panic!("plugin crashed!");
        }
    }

    #[test]
    fn test_panicking_plugin_does_not_crash_bus() {
        let bus = EventBus::new();

        // Register a panicking plugin before a normal one.
        let panicker = Arc::new(PanickingPlugin::new("bad-plugin", &["file.discovered"]));
        let normal = Arc::new(TestPlugin::new("good-plugin", &["file.discovered"]));

        bus.subscribe_plugin(panicker, 0);
        bus.subscribe_plugin(normal, 10);

        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: "/test.mkv".into(),
            size: 1024,
            content_hash: "abc123".to_string(),
        });

        let results = bus.publish(event);
        // The panicking plugin produces a PluginError result; the normal plugin produces its result.
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "bad-plugin");
        assert_eq!(results[0].produced_events.len(), 1);
        assert_eq!(results[0].produced_events[0].event_type(), "plugin.error");
        assert_eq!(results[1].plugin_name, "good-plugin");
    }

    // --- Event cascading tests ---

    /// Plugin that produces a new event when it handles an event.
    struct CascadingPlugin {
        name: String,
        handled_types: Vec<String>,
        produced_event: Option<Event>,
    }

    impl CascadingPlugin {
        fn new(name: &str, types: &[&str], produced: Option<Event>) -> Self {
            Self {
                name: name.to_string(),
                handled_types: types.iter().map(|s| s.to_string()).collect(),
                produced_event: produced,
            }
        }
    }

    impl Plugin for CascadingPlugin {
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
                produced_events: self.produced_event.iter().cloned().collect(),
                data: None,
            }))
        }
    }

    #[test]
    fn test_event_cascading() {
        let bus = EventBus::new();

        // Plugin A handles "file.discovered" and produces "file.introspected".
        let introspected_event = Event::FileIntrospected(FileIntrospectedEvent {
            file: MediaFile::new("/test.mkv".into()),
        });
        let plugin_a = Arc::new(CascadingPlugin::new(
            "introspector",
            &["file.discovered"],
            Some(introspected_event),
        ));

        // Plugin B handles "file.introspected" (no produced events).
        let plugin_b = Arc::new(CascadingPlugin::new("store", &["file.introspected"], None));

        bus.subscribe_plugin(plugin_a, 0);
        bus.subscribe_plugin(plugin_b, 10);

        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: "/test.mkv".into(),
            size: 1024,
            content_hash: "abc123".to_string(),
        });

        let results = bus.publish(event);
        // Should have 2 results: introspector (from original) + store (from cascaded).
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].plugin_name, "introspector");
        assert_eq!(results[1].plugin_name, "store");
    }

    #[test]
    fn test_cascade_depth_limit_prevents_infinite_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Plugin that produces the same event type it handles, creating an infinite loop.
        struct InfiniteLoopPlugin {
            call_count: AtomicUsize,
        }

        impl Plugin for InfiniteLoopPlugin {
            fn name(&self) -> &str {
                "loop-plugin"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
            fn handles(&self, event_type: &str) -> bool {
                event_type == "tool.detected"
            }
            fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                Ok(Some(EventResult {
                    plugin_name: "loop-plugin".to_string(),
                    produced_events: vec![Event::ToolDetected(ToolDetectedEvent {
                        tool_name: "ffprobe".into(),
                        version: "6.0".into(),
                        path: "/usr/bin/ffprobe".into(),
                    })],
                    data: None,
                }))
            }
        }

        let bus = EventBus::new();
        let plugin = Arc::new(InfiniteLoopPlugin {
            call_count: AtomicUsize::new(0),
        });
        let plugin_ref = plugin.clone();
        bus.subscribe_plugin(plugin, 0);

        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.0".into(),
            path: "/usr/bin/ffprobe".into(),
        });

        let results = bus.publish(event);

        // Should have been called exactly MAX_CASCADE_DEPTH + 1 times (depth 0 through 8).
        let count = plugin_ref.call_count.load(Ordering::SeqCst);
        assert_eq!(count, (super::MAX_CASCADE_DEPTH as usize) + 1);
        assert_eq!(results.len(), (super::MAX_CASCADE_DEPTH as usize) + 1);
    }
}
