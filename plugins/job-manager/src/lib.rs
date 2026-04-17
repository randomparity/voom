//! Job Manager Plugin for VOOM.
//!
//! Provides background job processing with:
//! - Priority-based job queue backed by `JobStorage`
//! - Configurable concurrent worker pool (tokio tasks)
//! - Job lifecycle management: enqueue, claim, progress, complete, fail, cancel
//! - Pluggable progress reporting (CLI, database, custom)
//! - Batch processing with error handling strategies

pub mod progress;
pub mod queue;
pub mod worker;

use std::sync::Arc;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::storage::JobStorage;
use voom_kernel::{Plugin, PluginContext};

/// The job manager plugin.
///
/// Manages background job processing with a priority queue and worker pool.
/// Jobs are persisted via `JobStorage`, enabling recovery after crashes.
pub struct JobManagerPlugin {
    queue: Option<Arc<queue::JobQueue>>,
    capabilities: Vec<Capability>,
}

impl JobManagerPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            queue: None,
            capabilities: vec![Capability::ManageJobs],
        }
    }

    /// Initialize with a storage backend.
    pub fn from_store(store: Arc<dyn JobStorage>) -> Self {
        Self {
            queue: Some(Arc::new(queue::JobQueue::new(store))),
            capabilities: vec![Capability::ManageJobs],
        }
    }

    #[must_use]
    pub fn queue(&self) -> Option<&Arc<queue::JobQueue>> {
        self.queue.as_ref()
    }
}

impl Default for JobManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for JobManagerPlugin {
    fn name(&self) -> &'static str {
        "job-manager"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        matches!(
            event_type,
            Event::JOB_STARTED
                | Event::JOB_PROGRESS
                | Event::JOB_COMPLETED
                | Event::JOB_ENQUEUE_REQUESTED
        )
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        self.queue = ctx.resource::<queue::JobQueue>();
        tracing::info!(has_queue = self.queue.is_some(), "job-manager initialized");
        Ok(vec![])
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::JobEnqueueRequested(e) => {
                if let Some(queue) = &self.queue {
                    let job_id =
                        queue.enqueue(e.job_type.clone(), e.priority, e.payload.clone())?;
                    tracing::info!(
                        job_id = %job_id,
                        job_type = %e.job_type,
                        requester = %e.requester,
                        "enqueued job via event"
                    );
                } else {
                    tracing::warn!(
                        job_type = %e.job_type,
                        requester = %e.requester,
                        "no job queue available, cannot enqueue"
                    );
                }
                Ok(None)
            }
            Event::JobStarted(e) => {
                tracing::info!(job_id = %e.job_id, desc = %e.description, "Job started");
                Ok(None)
            }
            Event::JobProgress(e) => {
                tracing::debug!(
                    job_id = %e.job_id,
                    progress = format!("{:.1}%", e.progress * 100.0),
                    "Job progress"
                );
                Ok(None)
            }
            Event::JobCompleted(e) => {
                if e.success {
                    tracing::info!(job_id = %e.job_id, "Job completed successfully");
                } else {
                    tracing::warn!(
                        job_id = %e.job_id,
                        message = e.message.as_deref().unwrap_or("unknown"),
                        "Job failed"
                    );
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::{JobCompletedEvent, JobProgressEvent, JobStartedEvent};
    use voom_domain::test_support::InMemoryStore;

    #[test]
    fn test_plugin_metadata() {
        let plugin = JobManagerPlugin::new();
        assert_eq!(plugin.name(), "job-manager");
        assert!(!plugin.version().is_empty());
        assert_eq!(plugin.capabilities().len(), 1);
    }

    #[test]
    fn test_plugin_handles_events() {
        let plugin = JobManagerPlugin::new();
        assert!(plugin.handles(Event::JOB_STARTED));
        assert!(plugin.handles(Event::JOB_PROGRESS));
        assert!(plugin.handles(Event::JOB_COMPLETED));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
    }

    #[test]
    fn test_plugin_from_store() {
        let store = Arc::new(InMemoryStore::new());
        let plugin = JobManagerPlugin::from_store(store);
        assert!(plugin.queue().is_some());
    }

    #[test]
    fn test_on_event_job_started() {
        let plugin = JobManagerPlugin::new();
        let event = Event::JobStarted(JobStartedEvent::new(
            uuid::Uuid::new_v4(),
            "Processing file",
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_job_progress() {
        let plugin = JobManagerPlugin::new();
        let event = Event::JobProgress({
            let mut e = JobProgressEvent::new(uuid::Uuid::new_v4(), 0.5);
            e.message = Some("Halfway".into());
            e
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_job_completed() {
        let plugin = JobManagerPlugin::new();

        let event = Event::JobCompleted({
            let mut e = JobCompletedEvent::new(uuid::Uuid::new_v4(), true);
            e.message = None;
            e
        });
        assert!(plugin.on_event(&event).unwrap().is_none());

        let event = Event::JobCompleted({
            let mut e = JobCompletedEvent::new(uuid::Uuid::new_v4(), false);
            e.message = Some("Encoder error".into());
            e
        });
        assert!(plugin.on_event(&event).unwrap().is_none());
    }

    #[test]
    fn test_handles_job_enqueue_requested() {
        let plugin = JobManagerPlugin::new();
        assert!(plugin.handles(Event::JOB_ENQUEUE_REQUESTED));
    }

    #[test]
    fn test_on_event_enqueue_requested_with_queue() {
        use voom_domain::events::JobEnqueueRequestedEvent;
        use voom_domain::job::JobType;
        use voom_domain::storage::JobStorage;

        let store = Arc::new(InMemoryStore::new());
        let plugin = JobManagerPlugin::from_store(store.clone());

        let event = Event::JobEnqueueRequested(JobEnqueueRequestedEvent::new(
            JobType::Introspect,
            50,
            Some(serde_json::json!({"path": "/media/test.mkv"})),
            "ffprobe-introspector",
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());

        let jobs = store
            .list_jobs(&voom_domain::storage::JobFilters::default())
            .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, JobType::Introspect);
        assert_eq!(jobs[0].priority, 50);
    }

    #[test]
    fn test_on_event_enqueue_requested_without_queue() {
        use voom_domain::events::JobEnqueueRequestedEvent;
        use voom_domain::job::JobType;

        let plugin = JobManagerPlugin::new();
        let event = Event::JobEnqueueRequested(JobEnqueueRequestedEvent::new(
            JobType::Scan,
            100,
            None,
            "test-requester",
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_unhandled() {
        let plugin = JobManagerPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffmpeg",
            "6.1",
            std::path::PathBuf::from("/usr/bin/ffmpeg"),
        ));
        assert!(plugin.on_event(&event).unwrap().is_none());
    }
}
