//! Bridges kernel event-bus events to the web server's SSE broadcast channel.
//!
//! The web server is library-only and cannot subscribe to the kernel bus
//! directly. This plugin holds a clone of the web server's
//! [`broadcast::Sender<SseEvent>`] and forwards relevant kernel events
//! (job lifecycle, file introspection) to it so that browser SSE clients
//! receive live updates.

use tokio::sync::broadcast;
use tracing::{debug, trace};

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

    /// Extract the basename of a path as a `String`. Returns an empty
    /// string if the path has no file-name component (e.g. `/`).
    fn basename(path: &std::path::Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
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
                path: Self::basename(&e.file.path),
            }),
            Event::PlanExecuting(e) => Some(SseEvent::PlanExecuting {
                plan_id: e.plan_id.to_string(),
                file: Self::basename(&e.path),
                phase: e.phase_name.clone(),
                action_count: e.action_count,
            }),
            Event::PlanCompleted(e) => Some(SseEvent::PlanCompleted {
                plan_id: e.plan_id.to_string(),
                file: Self::basename(&e.path),
                phase: e.phase_name.clone(),
                actions_applied: e.actions_applied,
            }),
            Event::PlanSkipped(e) => Some(SseEvent::PlanSkipped {
                plan_id: e.plan_id.to_string(),
                file: Self::basename(&e.path),
                phase: e.phase_name.clone(),
                skip_reason: e.skip_reason.clone(),
            }),
            Event::PlanFailed(e) => Some(SseEvent::PlanFailed {
                plan_id: e.plan_id.to_string(),
                file: Self::basename(&e.path),
                phase: e.phase_name.clone(),
                error: e.error.clone(),
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
                | Event::PLAN_EXECUTING
                | Event::PLAN_COMPLETED
                | Event::PLAN_SKIPPED
                | Event::PLAN_FAILED
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let Some(sse_event) = Self::to_sse_event(event) else {
            return Ok(None);
        };

        let event_kind = event.event_type();
        trace!(event_kind, "forwarding kernel event to SSE channel");

        // `send` returns Err only when there are zero receivers, which is the
        // normal case while no SSE clients are connected. Log at debug level
        // so operators can correlate "no events delivered" with "no clients".
        if self.sse_tx.send(sse_event).is_err() {
            debug!(event_kind, "SSE send dropped: no active subscribers");
        }
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
    fn forwards_plan_executing_with_basename_only() {
        use voom_domain::events::PlanExecutingEvent;
        let (bridge, mut rx) = bridge_with_rx();
        let plan_id = Uuid::new_v4();
        let event = Event::PlanExecuting(PlanExecutingEvent::new(
            plan_id,
            PathBuf::from("/media/movies/MyMovie.mkv"),
            "transcode",
            3,
        ));

        bridge.on_event(&event).unwrap();

        let sse = rx.try_recv().expect("event should be broadcast");
        match sse {
            SseEvent::PlanExecuting {
                plan_id: pid,
                file,
                phase,
                action_count,
            } => {
                assert_eq!(pid, plan_id.to_string());
                assert_eq!(file, "MyMovie.mkv");
                assert_eq!(phase, "transcode");
                assert_eq!(action_count, 3);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_plan_completed() {
        use voom_domain::events::PlanCompletedEvent;
        let (bridge, mut rx) = bridge_with_rx();
        let plan_id = Uuid::new_v4();
        let event = Event::PlanCompleted(PlanCompletedEvent::new(
            plan_id,
            PathBuf::from("/m/x.mkv"),
            "remux",
            5,
            false,
        ));
        bridge.on_event(&event).unwrap();
        match rx.try_recv().unwrap() {
            SseEvent::PlanCompleted {
                plan_id: pid,
                file,
                phase,
                actions_applied,
            } => {
                assert_eq!(pid, plan_id.to_string());
                assert_eq!(file, "x.mkv");
                assert_eq!(phase, "remux");
                assert_eq!(actions_applied, 5);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_plan_skipped() {
        use voom_domain::events::PlanSkippedEvent;
        let (bridge, mut rx) = bridge_with_rx();
        let plan_id = Uuid::new_v4();
        let event = Event::PlanSkipped(PlanSkippedEvent::new(
            plan_id,
            PathBuf::from("/m/x.mkv"),
            "transcode",
            "no matching tracks",
        ));
        bridge.on_event(&event).unwrap();
        match rx.try_recv().unwrap() {
            SseEvent::PlanSkipped {
                plan_id: pid,
                file,
                phase,
                skip_reason,
            } => {
                assert_eq!(pid, plan_id.to_string());
                assert_eq!(file, "x.mkv");
                assert_eq!(phase, "transcode");
                assert_eq!(skip_reason, "no matching tracks");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn forwards_plan_failed_without_leaking_chain_or_detail() {
        use voom_domain::events::PlanFailedEvent;
        let (bridge, mut rx) = bridge_with_rx();
        let plan_id = Uuid::new_v4();
        let mut payload = PlanFailedEvent::new(
            plan_id,
            PathBuf::from("/m/x.mkv"),
            "transcode",
            "ffmpeg returned non-zero",
        );
        // These fields exist but must NOT appear in the SSE payload —
        // they may contain stack traces or absolute paths.
        payload.error_chain = vec!["secret root cause".into()];
        let event = Event::PlanFailed(payload);

        bridge.on_event(&event).unwrap();
        let sse = rx.try_recv().unwrap();

        // Structural check: the destructure only binds the 4 allowed fields.
        match &sse {
            SseEvent::PlanFailed {
                plan_id: pid,
                file,
                phase,
                error,
            } => {
                assert_eq!(pid, &plan_id.to_string());
                assert_eq!(file, "x.mkv");
                assert_eq!(phase, "transcode");
                assert_eq!(error, "ffmpeg returned non-zero");
            }
            other => panic!("unexpected variant: {other:?}"),
        }

        // Runtime check: defense-in-depth. If a future refactor adds
        // error_chain/execution_detail back to SseEvent::PlanFailed and
        // populates it from the bridge, this assertion catches the
        // serialized JSON leak even if the destructure above still
        // compiles and passes.
        let json = serde_json::to_string(&sse).expect("SSE event must serialize");
        assert!(
            !json.contains("error_chain"),
            "PlanFailed SSE payload must not contain error_chain: {json}"
        );
        assert!(
            !json.contains("execution_detail"),
            "PlanFailed SSE payload must not contain execution_detail: {json}"
        );
        assert!(
            !json.contains("secret root cause"),
            "PlanFailed SSE payload must not leak error_chain contents: {json}"
        );
    }

    #[test]
    fn handles_returns_true_for_all_plan_lifecycle_events() {
        let (bridge, _rx) = bridge_with_rx();
        assert!(bridge.handles(Event::PLAN_EXECUTING));
        assert!(bridge.handles(Event::PLAN_COMPLETED));
        assert!(bridge.handles(Event::PLAN_SKIPPED));
        assert!(bridge.handles(Event::PLAN_FAILED));
    }

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
