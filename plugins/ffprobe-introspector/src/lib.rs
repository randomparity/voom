//! `FFprobe` introspection plugin: media file analysis via ffprobe JSON output.
//!
//! This plugin serves dual roles:
//! - **Kernel-registered plugin** — subscribes to `FileDiscovered` events and
//!   enqueues `JobType::Introspect` jobs via the job queue.
//! - **Direct-call library** — the `introspect()` method is called directly by
//!   the CLI for deterministic progress reporting.

pub mod ffprobe;
pub mod parser;

use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult, FileIntrospectedEvent, JobEnqueueRequestedEvent};
use voom_domain::job::{DiscoveredFilePayload, JobType};
use voom_kernel::{Plugin, PluginContext};

/// `FFprobe` introspector: extracts media metadata using ffprobe.
pub struct FfprobeIntrospectorPlugin {
    ffprobe_path: String,
    timeout: Duration,
    capabilities: Vec<Capability>,
}

impl FfprobeIntrospectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ffprobe_path: "ffprobe".into(),
            timeout: Duration::from_secs(60),
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
        content_hash: Option<&str>,
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

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::FILE_DISCOVERED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        if let Event::FileDiscovered(e) = event {
            let payload = serde_json::to_value(DiscoveredFilePayload {
                path: e.path.to_string_lossy().into_owned(),
                size: e.size,
                content_hash: e.content_hash.clone(),
            })
            .map_err(|err| {
                VoomError::plugin(
                    "ffprobe-introspector",
                    format!("failed to serialize DiscoveredFilePayload: {err}"),
                )
            })?;
            let enqueue_event = Event::JobEnqueueRequested(JobEnqueueRequestedEvent::new(
                JobType::Introspect,
                50,
                Some(payload),
                "ffprobe-introspector",
            ));
            let mut result = EventResult::new("ffprobe-introspector");
            result.produced_events = vec![enqueue_event];
            tracing::info!(
                path = %e.path.display(),
                "produced JobEnqueueRequested for introspection"
            );
            return Ok(Some(result));
        }
        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        match ctx.parse_config::<serde_json::Value>() {
            Ok(config) => {
                if let Some(path) = config.get("ffprobe_path").and_then(|v| v.as_str()) {
                    self.ffprobe_path = path.to_string();
                }
            }
            Err(e) => {
                tracing::warn!("ffprobe-introspector config parse failed, using defaults: {e}");
            }
        }

        tracing::info!(
            ffprobe_path = %self.ffprobe_path,
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
    fn test_on_event_produces_enqueue_event() {
        let plugin = FfprobeIntrospectorPlugin::new();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            Some("abc123".into()),
        ));
        let result = plugin
            .on_event(&event)
            .unwrap()
            .expect("should produce result");
        assert_eq!(result.produced_events.len(), 1);

        let produced = &result.produced_events[0];
        assert_eq!(produced.event_type(), Event::JOB_ENQUEUE_REQUESTED);
        if let Event::JobEnqueueRequested(e) = produced {
            assert_eq!(e.job_type, JobType::Introspect);
            assert_eq!(e.priority, 50);
            assert_eq!(e.requester, "ffprobe-introspector");
            assert!(e.payload.is_some());
        } else {
            panic!("expected JobEnqueueRequested event");
        }
    }

    #[test]
    fn test_on_event_ignores_other_events() {
        let plugin = FfprobeIntrospectorPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.1",
            PathBuf::from("/usr/bin/ffprobe"),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
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
