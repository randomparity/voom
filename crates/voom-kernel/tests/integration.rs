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
        event_type == Event::FILE_DISCOVERED
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
                claimed: false,
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
        event_type == Event::FILE_DISCOVERED
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::FileDiscovered(discovered) = event {
            // Produce a FileIntrospected event in response
            let mut file = voom_domain::media::MediaFile::new(discovered.path.clone());
            file.container = voom_domain::media::Container::Mkv;
            let introspected = Event::FileIntrospected(FileIntrospectedEvent { file });
            Ok(Some(EventResult {
                plugin_name: "mock-introspector".to_string(),
                produced_events: vec![introspected],
                data: None,
                claimed: false,
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
        event_type == Event::PLAN_CREATED
    }

    fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        Ok(Some(EventResult {
            plugin_name: "mock-mkvtoolnix".to_string(),
            produced_events: vec![],
            data: Some(serde_json::json!({"executed": true})),
            claimed: false,
        }))
    }
}

#[test]
fn test_kernel_register_and_dispatch() {
    let mut kernel = Kernel::new();

    // Register native plugins.
    let logger = Arc::new(DiscoveryLogger {
        name: "discovery-logger".into(),
    });
    let introspector = Arc::new(MockIntrospector);

    kernel.register_plugin(logger, 0);
    kernel.register_plugin(introspector, 10);

    assert_eq!(kernel.registry.len(), 2);
    assert_eq!(kernel.subscriber_count(), 2);

    // Dispatch a FileDiscovered event.
    let event = Event::FileDiscovered(FileDiscoveredEvent {
        path: "/media/movies/test.mkv".into(),
        size: 1_500_000_000,
        content_hash: "xxh64:abc123def456".to_string(),
    });

    let results = kernel.dispatch(event);

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
        Event::FILE_INTROSPECTED
    );
}

#[test]
fn test_kernel_capability_queries() {
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

#[test]
fn test_event_cascading() {
    let mut kernel = Kernel::new();

    let introspector = Arc::new(MockIntrospector);
    kernel.register_plugin(introspector, 0);

    // Dispatch initial event.
    let event = Event::FileDiscovered(FileDiscoveredEvent {
        path: "/media/test.mkv".into(),
        size: 500_000,
        content_hash: "xxh64:000".to_string(),
    });

    let results = kernel.dispatch(event);
    assert_eq!(results.len(), 1);

    // The introspector produced a new event — cascade it.
    let produced = &results[0].produced_events;
    assert_eq!(produced.len(), 1);

    // Dispatch the produced event (nothing handles file.introspected yet).
    let cascade_results = kernel.dispatch(produced[0].clone());
    assert!(cascade_results.is_empty());
}

// --- Executor double-dispatch tests ---

/// Mock MKV-only executor that claims events it handles.
struct MockMkvExecutor;

impl Plugin for MockMkvExecutor {
    fn name(&self) -> &str {
        "mock-mkv-executor"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn capabilities(&self) -> &[Capability] {
        &[]
    }
    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED
    }
    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::PlanCreated(plan_event) = event {
            let is_mkv = plan_event.plan.file.container == voom_domain::media::Container::Mkv;

            let has_transcode = plan_event.plan.actions.iter().any(|a| {
                matches!(
                    a.operation,
                    voom_domain::plan::OperationType::TranscodeVideo
                        | voom_domain::plan::OperationType::TranscodeAudio
                )
            });

            // Handle MKV files with non-transcode operations only
            if is_mkv && !has_transcode {
                return Ok(Some(EventResult {
                    plugin_name: "mock-mkv-executor".into(),
                    produced_events: vec![],
                    data: Some(serde_json::json!({"handler": "mkvtoolnix"})),
                    claimed: true,
                }));
            }
        }
        Ok(None)
    }
}

/// Mock FFmpeg executor that handles all plans and claims them.
struct MockFfmpegExecutor;

impl Plugin for MockFfmpegExecutor {
    fn name(&self) -> &str {
        "mock-ffmpeg-executor"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn capabilities(&self) -> &[Capability] {
        &[]
    }
    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::PLAN_CREATED
    }
    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        if let Event::PlanCreated(_) = event {
            Ok(Some(EventResult {
                plugin_name: "mock-ffmpeg-executor".into(),
                produced_events: vec![],
                data: Some(serde_json::json!({"handler": "ffmpeg"})),
                claimed: true,
            }))
        } else {
            Ok(None)
        }
    }
}

fn make_plan(
    container: voom_domain::media::Container,
    actions: Vec<voom_domain::plan::PlannedAction>,
) -> voom_domain::plan::Plan {
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    let mut file = MediaFile::new(std::path::PathBuf::from("/media/test.mkv"));
    file.container = container;
    Plan {
        id: uuid::Uuid::new_v4(),
        file,
        policy_name: "test".into(),
        phase_name: "normalize".into(),
        actions,
        warnings: vec![],
        skip_reason: None,
        policy_hash: None,
        evaluated_at: chrono::Utc::now(),
    }
}

#[test]
fn test_executor_claimed_mkv_metadata_goes_to_mkvtoolnix() {
    use voom_domain::plan::{OperationType, PlannedAction};

    let mut kernel = Kernel::new();

    // MKV executor at priority 39, FFmpeg at 40
    kernel.register_plugin(Arc::new(MockMkvExecutor), 39);
    kernel.register_plugin(Arc::new(MockFfmpegExecutor), 40);

    // MKV file with metadata-only actions
    let plan = make_plan(
        voom_domain::media::Container::Mkv,
        vec![PlannedAction {
            operation: OperationType::SetDefault,
            track_index: Some(1),
            parameters: voom_domain::plan::ActionParams::Empty,
            description: "Set default".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    // Only mkvtoolnix should handle (claims the event)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].plugin_name, "mock-mkv-executor");
    assert!(results[0].claimed);
}

#[test]
fn test_executor_claimed_mp4_goes_to_ffmpeg() {
    use voom_domain::plan::{ActionParams, OperationType, PlannedAction};

    let mut kernel = Kernel::new();

    kernel.register_plugin(Arc::new(MockMkvExecutor), 39);
    kernel.register_plugin(Arc::new(MockFfmpegExecutor), 40);

    // MP4 file — mkvtoolnix declines, ffmpeg handles
    let plan = make_plan(
        voom_domain::media::Container::Mp4,
        vec![PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: ActionParams::Transcode {
                codec: "hevc".into(),
                settings: serde_json::json!({}),
            },
            description: "Transcode".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].plugin_name, "mock-ffmpeg-executor");
}

#[test]
fn test_executor_mkv_transcode_falls_through_to_ffmpeg() {
    use voom_domain::plan::{ActionParams, OperationType, PlannedAction};

    let mut kernel = Kernel::new();

    kernel.register_plugin(Arc::new(MockMkvExecutor), 39);
    kernel.register_plugin(Arc::new(MockFfmpegExecutor), 40);

    // MKV file with transcode — mkvtoolnix declines, ffmpeg handles
    let plan = make_plan(
        voom_domain::media::Container::Mkv,
        vec![PlannedAction {
            operation: OperationType::TranscodeVideo,
            track_index: Some(0),
            parameters: ActionParams::Transcode {
                codec: "h264".into(),
                settings: serde_json::json!({}),
            },
            description: "Transcode to H.264".into(),
        }],
    );

    let event = Event::PlanCreated(PlanCreatedEvent { plan });
    let results = kernel.dispatch(event);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].plugin_name, "mock-ffmpeg-executor");
}
