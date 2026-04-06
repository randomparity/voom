//! Bridges kernel event-bus events to the web server's SSE broadcast channel.
//!
//! The web server is library-only and cannot subscribe to the kernel bus
//! directly. This plugin holds a clone of the web server's
//! [`broadcast::Sender<SseEvent>`] and forwards relevant kernel events
//! (job lifecycle, file introspection) to it so that browser SSE clients
//! receive live updates.

use tokio::sync::broadcast;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_kernel::Plugin;
use voom_web_server::state::SseEvent;

/// Plugin that forwards kernel events to the web server's SSE broadcast channel.
pub struct WebSseBridgePlugin {
    sse_tx: broadcast::Sender<SseEvent>,
}

impl WebSseBridgePlugin {
    /// Construct a bridge that forwards events to the given SSE sender.
    #[must_use]
    pub fn new(sse_tx: broadcast::Sender<SseEvent>) -> Self {
        Self { sse_tx }
    }

    /// Map a kernel event to its SSE counterpart, if one exists.
    ///
    /// Returns `None` for event types the bridge does not forward.
    fn to_sse_event(event: &Event) -> Option<SseEvent> {
        match event {
            Event::JobStarted(e) => Some(SseEvent::JobStarted {
                job_id: e.job_id.to_string(),
                description: e.description.clone(),
            }),
            Event::JobProgress(e) => Some(SseEvent::JobProgress {
                job_id: e.job_id.to_string(),
                progress: e.progress,
                message: e.message.clone(),
            }),
            Event::JobCompleted(e) => Some(SseEvent::JobCompleted {
                job_id: e.job_id.to_string(),
                success: e.success,
                message: e.message.clone(),
            }),
            Event::FileIntrospected(e) => Some(SseEvent::FileIntrospected {
                path: e
                    .file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }),
            _ => None,
        }
    }
}

impl Plugin for WebSseBridgePlugin {
    fn name(&self) -> &str {
        "web-sse-bridge"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        matches!(
            event_type,
            Event::JOB_STARTED
                | Event::JOB_PROGRESS
                | Event::JOB_COMPLETED
                | Event::FILE_INTROSPECTED
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let Some(sse_event) = Self::to_sse_event(event) else {
            return Ok(None);
        };

        // `send` returns Err only when there are zero receivers, which is the
        // normal case while no SSE clients are connected. Drop the result.
        let _ = self.sse_tx.send(sse_event);
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;
    use voom_domain::events::{
        FileIntrospectedEvent, JobCompletedEvent, JobProgressEvent, JobStartedEvent,
    };
    use voom_domain::media::MediaFile;

    fn bridge_with_rx() -> (WebSseBridgePlugin, broadcast::Receiver<SseEvent>) {
        let (tx, rx) = broadcast::channel(16);
        (WebSseBridgePlugin::new(tx), rx)
    }

    #[test]
    fn handles_only_forwarded_event_types() {
        let (bridge, _rx) = bridge_with_rx();
        assert!(bridge.handles(Event::JOB_STARTED));
        assert!(bridge.handles(Event::JOB_PROGRESS));
        assert!(bridge.handles(Event::JOB_COMPLETED));
        assert!(bridge.handles(Event::FILE_INTROSPECTED));

        assert!(!bridge.handles(Event::FILE_DISCOVERED));
        assert!(!bridge.handles(Event::PLAN_CREATED));
        assert!(!bridge.handles(Event::TOOL_DETECTED));
    }

    #[test]
    fn forwards_job_started() {
        let (bridge, mut rx) = bridge_with_rx();
        let job_id = Uuid::new_v4();
        let event = Event::JobStarted(JobStartedEvent::new(job_id, "scan /media"));

        bridge.on_event(&event).unwrap();

        let sse = rx.try_recv().expect("event should be broadcast");
        match sse {
            SseEvent::JobStarted {
                job_id: id,
                description,
            } => {
                assert_eq!(id, job_id.to_string());
                assert_eq!(description, "scan /media");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_job_progress_with_message() {
        let (bridge, mut rx) = bridge_with_rx();
        let job_id = Uuid::new_v4();
        let mut payload = JobProgressEvent::new(job_id, 0.42);
        payload.message = Some("transcoding".into());
        let event = Event::JobProgress(payload);

        bridge.on_event(&event).unwrap();

        let sse = rx.try_recv().unwrap();
        match sse {
            SseEvent::JobProgress {
                job_id: id,
                progress,
                message,
            } => {
                assert_eq!(id, job_id.to_string());
                assert!((progress - 0.42).abs() < f64::EPSILON);
                assert_eq!(message.as_deref(), Some("transcoding"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_job_completed() {
        let (bridge, mut rx) = bridge_with_rx();
        let job_id = Uuid::new_v4();
        let event = Event::JobCompleted(JobCompletedEvent::new(job_id, true));

        bridge.on_event(&event).unwrap();

        let sse = rx.try_recv().unwrap();
        match sse {
            SseEvent::JobCompleted {
                job_id: id,
                success,
                message,
            } => {
                assert_eq!(id, job_id.to_string());
                assert!(success);
                assert!(message.is_none());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_file_introspected_basename_only() {
        let (bridge, mut rx) = bridge_with_rx();
        let media = MediaFile::new(PathBuf::from("/media/movies/MyMovie.mkv"));
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(media));

        bridge.on_event(&event).unwrap();

        let sse = rx.try_recv().unwrap();
        match sse {
            SseEvent::FileIntrospected { path } => {
                assert_eq!(
                    path, "MyMovie.mkv",
                    "absolute filesystem path must not be exposed to SSE clients"
                );
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn file_introspected_with_no_basename_yields_empty_string() {
        // PathBuf::from("/") has no `file_name()` — make sure we don't panic
        // and don't accidentally fall back to the full path.
        let (bridge, mut rx) = bridge_with_rx();
        let media = MediaFile::new(PathBuf::from("/"));
        bridge
            .on_event(&Event::FileIntrospected(FileIntrospectedEvent::new(media)))
            .unwrap();
        let sse = rx.try_recv().unwrap();
        match sse {
            SseEvent::FileIntrospected { path } => assert_eq!(path, ""),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ignores_non_forwarded_events() {
        let (bridge, mut rx) = bridge_with_rx();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/media/x.mkv"),
            42,
            None,
        ));

        bridge.on_event(&event).unwrap();
        assert!(rx.try_recv().is_err(), "no event should be broadcast");
    }

    #[test]
    fn send_with_no_receivers_does_not_panic() {
        let (tx, rx) = broadcast::channel(4);
        drop(rx); // no subscribers
        let bridge = WebSseBridgePlugin::new(tx);
        let event = Event::JobStarted(JobStartedEvent::new(Uuid::new_v4(), "x"));
        bridge.on_event(&event).expect("must not error");
    }

    /// End-to-end: register the bridge with a real Kernel and dispatch a job
    /// event through the bus. The SSE receiver should observe the broadcast.
    #[test]
    fn integrates_with_kernel_dispatch() {
        use std::path::PathBuf;
        use std::sync::Arc;
        use voom_kernel::{Kernel, PluginContext};

        let (tx, mut rx) = broadcast::channel(8);
        let bridge = WebSseBridgePlugin::new(tx);

        let mut kernel = Kernel::new();
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        kernel
            .init_and_register(Arc::new(bridge), 200, &ctx)
            .expect("register bridge");

        let job_id = Uuid::new_v4();
        kernel.dispatch(Event::JobStarted(JobStartedEvent::new(
            job_id,
            "kernel test",
        )));

        let sse = rx.try_recv().expect("event should be broadcast");
        match sse {
            SseEvent::JobStarted {
                job_id: id,
                description,
            } => {
                assert_eq!(id, job_id.to_string());
                assert_eq!(description, "kernel test");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }
}
