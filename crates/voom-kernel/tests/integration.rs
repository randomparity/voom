use std::sync::Arc;
use voom_domain::capabilities::Capability;
use voom_domain::events::*;
use voom_kernel::{Kernel, Plugin};

/// A native plugin that logs file discovery events.
struct DiscoveryLogger {
    name: String,
}

impl Plugin for DiscoveryLogger {
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
        event_type == "file.discovered"
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::FileDiscovered(discovered) = event {
            Ok(Some(EventResult {
                plugin_name: self.name.clone(),
                produced_events: vec![],
                data: Some(serde_json::json!({
                    "logged_path": discovered.path.display().to_string(),
                    "size": discovered.size,
                })),
            }))
        } else {
            Ok(None)
        }
    }
}

/// A native plugin that simulates introspection of discovered files.
struct MockIntrospector;

impl Plugin for MockIntrospector {
    fn name(&self) -> &str {
        "mock-introspector"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> &[Capability] {
        // Use a static slice since we can't return a reference to a local vec.
        // In real code, this would be stored on the struct.
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "file.discovered"
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::FileDiscovered(discovered) = event {
            // Produce a FileIntrospected event in response
            let introspected = Event::FileIntrospected(FileIntrospectedEvent {
                path: discovered.path.clone(),
                container: "mkv".to_string(),
                track_count: 3,
            });
            Ok(Some(EventResult {
                plugin_name: "mock-introspector".to_string(),
                produced_events: vec![introspected],
                data: None,
            }))
        } else {
            Ok(None)
        }
    }
}

/// A native plugin with executor capabilities for testing capability queries.
struct MockExecutor {
    caps: Vec<Capability>,
}

impl Plugin for MockExecutor {
    fn name(&self) -> &str {
        "mock-mkvtoolnix"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> &[Capability] {
        &self.caps
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "plan.created"
    }

    fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        Ok(Some(EventResult {
            plugin_name: "mock-mkvtoolnix".to_string(),
            produced_events: vec![],
            data: Some(serde_json::json!({"executed": true})),
        }))
    }
}

#[tokio::test]
async fn test_kernel_register_and_dispatch() {
    let mut kernel = Kernel::new();

    // Register native plugins.
    let logger = Arc::new(DiscoveryLogger {
        name: "discovery-logger".into(),
    });
    let introspector = Arc::new(MockIntrospector);

    kernel.register_plugin(logger, 0);
    kernel.register_plugin(introspector, 10);

    assert_eq!(kernel.registry.len(), 2);
    assert_eq!(kernel.bus.subscriber_count(), 2);

    // Dispatch a FileDiscovered event.
    let event = Event::FileDiscovered(FileDiscoveredEvent {
        path: "/media/movies/test.mkv".into(),
        size: 1_500_000_000,
        content_hash: "xxh64:abc123def456".to_string(),
    });

    let results = kernel.dispatch(event).await;

    // Both plugins handle file.discovered.
    assert_eq!(results.len(), 2);

    // Logger should have captured the path.
    assert_eq!(results[0].plugin_name, "discovery-logger");
    let data = results[0].data.as_ref().unwrap();
    assert_eq!(data["logged_path"], "/media/movies/test.mkv");

    // Introspector should have produced a FileIntrospected event.
    assert_eq!(results[1].plugin_name, "mock-introspector");
    assert_eq!(results[1].produced_events.len(), 1);
    assert_eq!(
        results[1].produced_events[0].event_type(),
        "file.introspected"
    );
}

#[tokio::test]
async fn test_kernel_capability_queries() {
    let mut kernel = Kernel::new();

    let executor = Arc::new(MockExecutor {
        caps: vec![Capability::Execute {
            operations: vec!["metadata".into(), "reorder".into(), "remux".into()],
            formats: vec!["mkv".into()],
        }],
    });

    kernel.register_plugin(executor, 0);

    // Query by capability kind.
    let executors = kernel.registry.find_by_capability_kind("execute");
    assert_eq!(executors.len(), 1);
    assert_eq!(executors[0].name(), "mock-mkvtoolnix");

    // Query for specific operation + format.
    let handler = kernel.registry.find_for_operation("metadata", "mkv");
    assert!(handler.is_some());
    assert_eq!(handler.unwrap().name(), "mock-mkvtoolnix");

    // No handler for transcode.
    assert!(kernel
        .registry
        .find_for_operation("transcode", "mkv")
        .is_none());

    // No handler for mp4 format.
    assert!(kernel
        .registry
        .find_for_operation("metadata", "mp4")
        .is_none());
}

#[tokio::test]
async fn test_event_cascading() {
    let mut kernel = Kernel::new();

    let introspector = Arc::new(MockIntrospector);
    kernel.register_plugin(introspector, 0);

    // Dispatch initial event.
    let event = Event::FileDiscovered(FileDiscoveredEvent {
        path: "/media/test.mkv".into(),
        size: 500_000,
        content_hash: "xxh64:000".to_string(),
    });

    let results = kernel.dispatch(event).await;
    assert_eq!(results.len(), 1);

    // The introspector produced a new event — cascade it.
    let produced = &results[0].produced_events;
    assert_eq!(produced.len(), 1);

    // Dispatch the produced event (nothing handles file.introspected yet).
    let cascade_results = kernel.dispatch(produced[0].clone()).await;
    assert!(cascade_results.is_empty());
}
