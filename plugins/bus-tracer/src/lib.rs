//! Event bus tracer plugin: logs events to a file for development debugging.
//!
//! Subscribes to all (or filtered) event types and writes one JSON line per
//! event to a configurable output file. Useful for understanding event flow
//! during development.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;
use serde::Deserialize;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_kernel::{Plugin, PluginContext};

/// Configuration for the bus-tracer plugin, loaded from TOML.
#[derive(Debug, Deserialize)]
struct BusTracerConfig {
    /// Path to the output file (supports `~` expansion).
    #[serde(default = "default_output")]
    output: String,
    /// Glob patterns to filter which events to trace.
    /// Empty list = trace nothing (opt-in).
    #[serde(default)]
    filters: Vec<String>,
}

fn default_output() -> String {
    "~/.config/voom/event-trace.log".to_string()
}

impl Default for BusTracerConfig {
    fn default() -> Self {
        Self {
            output: default_output(),
            filters: Vec::new(),
        }
    }
}

/// Bus tracer plugin. Logs events to a file for development.
pub struct BusTracerPlugin {
    writer: Option<Arc<Mutex<File>>>,
    filters: Vec<String>,
}

impl BusTracerPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            writer: None,
            filters: Vec::new(),
        }
    }
}

impl Default for BusTracerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Match an event type string against a glob pattern.
/// Supports `*` as a suffix wildcard (e.g. `file.*` matches `file.discovered`).
fn glob_matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        event_type.starts_with(prefix) && event_type.as_bytes().get(prefix.len()) == Some(&b'.')
    } else {
        pattern == event_type
    }
}

/// Build a one-line summary of an event's payload for the trace log.
fn event_summary(event: &Event) -> String {
    match event {
        Event::FileDiscovered(e) => {
            format!("path={} size={}", e.path.display(), e.size)
        }
        Event::FileIntrospected(e) => {
            format!(
                "path={} tracks={}",
                e.file.path.display(),
                e.file.tracks.len()
            )
        }
        Event::FileIntrospectionFailed(e) => {
            format!("path={} error={}", e.path.display(), e.error)
        }
        Event::PlanCreated(e) => {
            format!(
                "phase={} actions={}",
                e.plan.phase_name,
                e.plan.actions.len()
            )
        }
        Event::PlanExecuting(e) => {
            format!("path={} phase={}", e.path.display(), e.phase_name)
        }
        Event::PlanCompleted(e) => {
            format!("path={} phase={}", e.path.display(), e.phase_name)
        }
        Event::PlanFailed(e) => {
            format!(
                "path={} phase={} error={}",
                e.path.display(),
                e.phase_name,
                e.error
            )
        }
        Event::JobStarted(e) => {
            format!("job_id={} desc={}", e.job_id, e.description)
        }
        Event::JobProgress(e) => {
            format!("job_id={} progress={:.1}%", e.job_id, e.progress * 100.0)
        }
        Event::JobCompleted(e) => {
            format!("job_id={} success={}", e.job_id, e.success)
        }
        Event::ToolDetected(e) => {
            format!("tool={} version={}", e.tool_name, e.version)
        }
        Event::MetadataEnriched(e) => {
            format!("path={} source={}", e.path.display(), e.source)
        }
        Event::ExecutorCapabilities(e) => {
            format!(
                "plugin={} decoders={} encoders={} formats={} hw_accels={}",
                e.plugin_name,
                e.codecs.decoders.len(),
                e.codecs.encoders.len(),
                e.formats.len(),
                e.hw_accels.len()
            )
        }
        Event::HealthStatus(e) => {
            format!("check={} passed={}", e.check_name, e.passed)
        }
        Event::PluginError(e) => {
            format!(
                "plugin={} event={} error={}",
                e.plugin_name, e.event_type, e.error
            )
        }
        _ => String::new(),
    }
}

/// Expand `~` at the start of a path to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn dirs_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

impl Plugin for BusTracerPlugin {
    fn name(&self) -> &str {
        "bus-tracer"
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
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        self.filters
            .iter()
            .any(|pattern| glob_matches(pattern, event_type))
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        let Some(writer) = &self.writer else {
            return Ok(None);
        };

        let entry = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "event": event.event_type(),
            "summary": event_summary(event),
        });

        let mut w = writer.lock();
        // Best-effort write; don't fail the event pipeline on IO errors
        if let Err(e) = writeln!(w, "{entry}") {
            tracing::warn!(error = %e, "bus-tracer failed to write event");
        }

        Ok(None)
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<Vec<Event>> {
        let config: BusTracerConfig = ctx.parse_config().unwrap_or_default();

        self.filters = config.filters;

        if self.filters.is_empty() {
            tracing::info!("bus-tracer has no filters configured, will not trace events");
            return Ok(vec![]);
        }

        let output_path = expand_tilde(&config.output);

        if let Some(parent) = output_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    error = %e,
                    path = %parent.display(),
                    "bus-tracer could not create output directory"
                );
                return Ok(vec![]);
            }
        }

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
        {
            Ok(file) => {
                tracing::info!(
                    path = %output_path.display(),
                    filters = ?self.filters,
                    "bus-tracer initialized"
                );
                self.writer = Some(Arc::new(Mutex::new(file)));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %output_path.display(),
                    "bus-tracer could not open output file"
                );
            }
        }

        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as IoRead;
    use voom_domain::events::FileDiscoveredEvent;

    #[test]
    fn test_glob_matches_exact() {
        assert!(glob_matches("file.discovered", "file.discovered"));
        assert!(!glob_matches("file.discovered", "file.introspected"));
    }

    #[test]
    fn test_glob_matches_wildcard() {
        assert!(glob_matches("file.*", "file.discovered"));
        assert!(glob_matches("file.*", "file.introspected"));
        assert!(!glob_matches("file.*", "plan.created"));
    }

    #[test]
    fn test_glob_matches_star_all() {
        assert!(glob_matches("*", "file.discovered"));
        assert!(glob_matches("*", "plan.created"));
    }

    #[test]
    fn test_glob_no_partial_prefix_match() {
        // "file.*" should not match "file_extra.discovered"
        assert!(!glob_matches("file.*", "fileX.discovered"));
    }

    #[test]
    fn test_handles_with_no_filters() {
        let plugin = BusTracerPlugin::new();
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("plan.created"));
    }

    #[test]
    fn test_handles_with_filters() {
        let mut plugin = BusTracerPlugin::new();
        plugin.filters = vec!["file.*".into(), "job.*".into()];

        assert!(plugin.handles("file.discovered"));
        assert!(plugin.handles("file.introspected"));
        assert!(plugin.handles("job.started"));
        assert!(!plugin.handles("plan.created"));
    }

    #[test]
    fn test_on_event_writes_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("trace.log");

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .unwrap();

        let mut plugin = BusTracerPlugin::new();
        plugin.writer = Some(Arc::new(Mutex::new(file)));
        plugin.filters = vec!["file.*".into()];

        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            "abc123".into(),
        ));

        plugin.on_event(&event).unwrap();

        let mut contents = String::new();
        File::open(&output_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(parsed["event"], "file.discovered");
        assert!(parsed["summary"]
            .as_str()
            .unwrap()
            .contains("/media/test.mkv"));
    }

    #[test]
    fn test_on_event_without_writer_is_noop() {
        let plugin = BusTracerPlugin::new();
        let event = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            "abc123".into(),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_init_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("trace.log");

        let mut plugin = BusTracerPlugin::new();
        let config = serde_json::json!({
            "output": output_path.to_string_lossy(),
            "filters": ["file.*", "job.*"]
        });
        let ctx = PluginContext::new(config, dir.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        assert!(plugin.writer.is_some());
        assert_eq!(plugin.filters.len(), 2);
    }

    #[test]
    fn test_init_with_no_filters() {
        let dir = tempfile::tempdir().unwrap();

        let mut plugin = BusTracerPlugin::new();
        let config = serde_json::json!({});
        let ctx = PluginContext::new(config, dir.path().to_path_buf());
        plugin.init(&ctx).unwrap();

        // No filters = no writer opened
        assert!(plugin.writer.is_none());
        assert!(plugin.filters.is_empty());
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = BusTracerPlugin::new();
        assert_eq!(plugin.name(), "bus-tracer");
        assert!(!plugin.version().is_empty());
        assert!(plugin.capabilities().is_empty());
    }
}
