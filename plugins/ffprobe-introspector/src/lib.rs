//! `FFprobe` introspection plugin: media file analysis via ffprobe JSON output.
//!
//! This plugin serves dual roles:
//! - **Kernel-registered plugin** — subscribes to `FileDiscovered` events and
//!   enqueues `JobType::Introspect` jobs via the job queue.
//! - **Direct-call library** — the `introspect()` method is called directly by
//!   the CLI for deterministic progress reporting.

pub mod ffprobe;
pub mod parser;

use std::sync::Arc;
use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult, FileIntrospectedEvent};
use voom_domain::job::JobType;
use voom_job_manager::queue::JobQueue;
use voom_kernel::{Plugin, PluginContext};

/// `FFprobe` introspector: extracts media metadata using ffprobe.
pub struct FfprobeIntrospectorPlugin {
    ffprobe_path: String,
    timeout: Duration,
    queue: Option<Arc<JobQueue>>,
    capabilities: Vec<Capability>,
}

impl FfprobeIntrospectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ffprobe_path: "ffprobe".into(),
            timeout: Duration::from_secs(60),
            queue: None,
            capabilities: vec![Capability::Introspect {
                formats: vec![
                    "mkv".into(),
                    "mp4".into(),
                    "avi".into(),
                    "wmv".into(),
                    "flv".into(),
                    "mov".into(),
                    "ts".into(),
                ],
            }],
        }
    }

    /// Set a custom path to the ffprobe binary.
    #[must_use]
    pub fn with_ffprobe_path(mut self, path: impl Into<String>) -> Self {
        self.ffprobe_path = path.into();
        self
    }

    #[must_use]
    pub fn ffprobe_path(&self) -> &str {
        &self.ffprobe_path
    }

    /// Introspect a single file and return the event.
    pub fn introspect(
        &self,
        path: &std::path::Path,
        size: u64,
        content_hash: &str,
    ) -> Result<FileIntrospectedEvent> {
        let json = ffprobe::run_ffprobe(&self.ffprobe_path, path, self.timeout)?;
        let file = parser::parse_ffprobe_output(&json, path, size, content_hash)?;
        Ok(FileIntrospectedEvent::new(file))
    }
}

impl Default for FfprobeIntrospectorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for FfprobeIntrospectorPlugin {
    fn name(&self) -> &str {
        "ffprobe-introspector"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn description(&self) -> &str {
        env!("CARGO_PKG_DESCRIPTION")
    }

    fn author(&self) -> &str {
        env!("CARGO_PKG_AUTHORS")
    }

    fn license(&self) -> &str {
        env!("CARGO_PKG_LICENSE")
    }

    fn homepage(&self) -> &str {
        env!("CARGO_PKG_REPOSITORY")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::FILE_DISCOVERED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        if let Event::FileDiscovered(e) = event {
            if let Some(queue) = &self.queue {
                let payload = serde_json::json!({
                    "path": e.path.to_string_lossy(),
                    "size": e.size,
                    "content_hash": e.content_hash,
                });
                queue.enqueue(JobType::Introspect, 50, Some(payload))?;
                tracing::info!(
                    path = %e.path.display(),
                    "enqueued introspection job"
                );
            } else {
                tracing::debug!(
                    path = %e.path.display(),
                    "no job queue available, skipping introspection enqueue"
                );
            }
        }
        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        // Obtain the shared job queue from the PluginContext resource map
        self.queue = ctx.resource::<JobQueue>();

        // Parse ffprobe path from plugin config if provided
        if let Ok(config) = ctx.parse_config::<serde_json::Value>() {
            if let Some(path) = config.get("ffprobe_path").and_then(|v| v.as_str()) {
                self.ffprobe_path = path.to_string();
            }
        }

        tracing::info!(
            ffprobe_path = %self.ffprobe_path,
            has_queue = self.queue.is_some(),
            "ffprobe introspector initialized"
        );
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_default_creates_same_as_new() {
        let plugin = FfprobeIntrospectorPlugin::default();
        assert_eq!(plugin.ffprobe_path(), "ffprobe");
    }

    #[test]
    fn test_custom_ffprobe_path() {
        let plugin = FfprobeIntrospectorPlugin::new().with_ffprobe_path("/usr/local/bin/ffprobe");
        assert_eq!(plugin.ffprobe_path(), "/usr/local/bin/ffprobe");
    }

    #[test]
    fn test_plugin_name_and_version() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert_eq!(plugin.name(), "ffprobe-introspector");
        assert!(!plugin.version().is_empty());
    }

    #[test]
    fn test_handles_file_discovered() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert!(plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::FILE_INTROSPECTED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn test_on_event_without_queue_does_not_error() {
        let plugin = FfprobeIntrospectorPlugin::new();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            "abc123".into(),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_with_queue_enqueues_job() {
        let store = Arc::new(voom_domain::test_support::InMemoryStore::new());
        let queue = Arc::new(JobQueue::new(store.clone()));

        let mut plugin = FfprobeIntrospectorPlugin::new();
        plugin.queue = Some(queue);

        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            "abc123".into(),
        ));
        plugin.on_event(&event).unwrap();

        // Verify a job was enqueued
        use voom_domain::storage::JobStorage;
        let jobs = store
            .list_jobs(&voom_domain::storage::JobFilters::default())
            .unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].job_type, JobType::Introspect);
    }

    #[test]
    fn test_init_reads_ffprobe_path_from_config() {
        let mut plugin = FfprobeIntrospectorPlugin::new();
        let config = serde_json::json!({"ffprobe_path": "/custom/ffprobe"});
        let ctx = PluginContext::new(config, PathBuf::from("/tmp"));
        plugin.init(&ctx).unwrap();
        assert_eq!(plugin.ffprobe_path(), "/custom/ffprobe");
    }
}
