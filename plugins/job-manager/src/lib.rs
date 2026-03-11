//! Job Manager Plugin for VOOM.
//!
//! Provides background job processing with:
//! - Priority-based job queue backed by StorageTrait
//! - Configurable concurrent worker pool (tokio tasks)
//! - Job lifecycle management: enqueue, claim, progress, complete, fail, cancel
//! - Pluggable progress reporting (CLI, database, custom)
//! - Batch processing with error handling strategies

pub mod progress;
pub mod queue;
pub mod worker;

#[cfg(test)]
pub(crate) mod test_helpers;

use std::sync::Arc;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::storage::StorageTrait;
use voom_kernel::{Plugin, PluginContext};

use crate::queue::JobQueue;

/// The job manager plugin.
///
/// Manages background job processing with a priority queue and worker pool.
/// Jobs are persisted via StorageTrait, enabling recovery after crashes.
pub struct JobManagerPlugin {
    queue: Option<Arc<JobQueue>>,
    capabilities: Vec<Capability>,
}

impl JobManagerPlugin {
    pub fn new() -> Self {
        Self {
            queue: None,
            capabilities: vec![Capability::ManageJobs],
        }
    }

    /// Initialize with a storage backend.
    pub fn with_store(store: Arc<dyn StorageTrait>) -> Self {
        Self {
            queue: Some(Arc::new(JobQueue::new(store))),
            capabilities: vec![Capability::ManageJobs],
        }
    }

    /// Get the job queue, if initialized.
    pub fn queue(&self) -> Option<&Arc<JobQueue>> {
        self.queue.as_ref()
    }
}

impl Default for JobManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for JobManagerPlugin {
    fn name(&self) -> &str {
        "job-manager"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        matches!(
            event_type,
            "job.started" | "job.progress" | "job.completed"
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
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

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        tracing::info!("Job manager plugin initialized");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::InMemoryStore;
    use voom_domain::events::{JobCompletedEvent, JobProgressEvent, JobStartedEvent};

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
        assert!(plugin.handles("job.started"));
        assert!(plugin.handles("job.progress"));
        assert!(plugin.handles("job.completed"));
        assert!(!plugin.handles("file.discovered"));
    }

    #[test]
    fn test_plugin_with_store() {
        let store = Arc::new(InMemoryStore::new());
        let plugin = JobManagerPlugin::with_store(store);
        assert!(plugin.queue().is_some());
    }

    #[test]
    fn test_on_event_job_started() {
        let plugin = JobManagerPlugin::new();
        let event = Event::JobStarted(JobStartedEvent {
            job_id: "test-1".into(),
            description: "Processing file".into(),
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_job_progress() {
        let plugin = JobManagerPlugin::new();
        let event = Event::JobProgress(JobProgressEvent {
            job_id: "test-1".into(),
            progress: 0.5,
            message: Some("Halfway".into()),
        });
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_job_completed() {
        let plugin = JobManagerPlugin::new();

        let event = Event::JobCompleted(JobCompletedEvent {
            job_id: "test-1".into(),
            success: true,
            message: None,
        });
        assert!(plugin.on_event(&event).unwrap().is_none());

        let event = Event::JobCompleted(JobCompletedEvent {
            job_id: "test-2".into(),
            success: false,
            message: Some("Encoder error".into()),
        });
        assert!(plugin.on_event(&event).unwrap().is_none());
    }

    #[test]
    fn test_on_event_unhandled() {
        let plugin = JobManagerPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent {
            tool_name: "ffmpeg".into(),
            version: "6.1".into(),
            path: std::path::PathBuf::from("/usr/bin/ffmpeg"),
        });
        assert!(plugin.on_event(&event).unwrap().is_none());
    }
}
