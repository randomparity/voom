//! `FFprobe` introspection plugin: media file analysis via ffprobe JSON output.
//!
//! Used as a direct-call library: the CLI invokes [`FfprobeIntrospectorPlugin::introspect`]
//! for each file with deterministic worker-pool concurrency. The plugin
//! registers no event subscriptions — no plugin consumes `JobType::Introspect`
//! jobs, so emitting them was wasted work.

pub mod ffprobe;
pub mod parser;

use std::time::Duration;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, FileIntrospectedEvent};
use voom_kernel::{Plugin, PluginContext};

/// `FFprobe` introspector: extracts media metadata using ffprobe.
pub struct FfprobeIntrospectorPlugin {
    ffprobe_path: String,
    timeout: Duration,
    available: bool,
    capabilities: Vec<Capability>,
}

impl FfprobeIntrospectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ffprobe_path: "ffprobe".into(),
            timeout: Duration::from_secs(60),
            available: false,
            capabilities: Self::default_capabilities(),
        }
    }

    fn default_capabilities() -> Vec<Capability> {
        vec![Capability::Introspect {
            formats: vec![
                "mkv".into(),
                "mp4".into(),
                "avi".into(),
                "wmv".into(),
                "flv".into(),
                "mov".into(),
                "ts".into(),
            ],
        }]
    }

    /// Probe whether the configured `ffprobe` binary is callable.
    fn detect_available(ffprobe_path: &str) -> bool {
        voom_process::run_with_timeout(ffprobe_path, &["-version"], Duration::from_secs(10))
            .is_ok_and(|o| o.status.success())
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
    fn name(&self) -> &'static str {
        "ffprobe-introspector"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        if self.available {
            &self.capabilities
        } else {
            &[]
        }
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

        self.available = Self::detect_available(&self.ffprobe_path);
        if !self.available {
            tracing::warn!(
                ffprobe_path = %self.ffprobe_path,
                "ffprobe not found; introspector will report no capabilities"
            );
        }

        tracing::info!(
            ffprobe_path = %self.ffprobe_path,
            available = self.available,
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
    fn test_handles_no_events() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::FILE_INTROSPECTED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn test_on_event_is_a_noop_for_file_discovered() {
        let plugin = FfprobeIntrospectorPlugin::new();
        let event = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
            PathBuf::from("/media/test.mkv"),
            1024,
            Some("abc123".into()),
        ));
        assert!(plugin.on_event(&event).unwrap().is_none());
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

    #[test]
    fn test_capabilities_empty_before_init() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert!(
            plugin.capabilities().is_empty(),
            "capabilities should be empty until init confirms ffprobe is present"
        );
    }

    #[test]
    fn test_capabilities_empty_when_ffprobe_missing() {
        let mut plugin = FfprobeIntrospectorPlugin::new();
        let config =
            serde_json::json!({"ffprobe_path": "/nonexistent/path/to/ffprobe-totally-missing"});
        let ctx = PluginContext::new(config, PathBuf::from("/tmp"));
        plugin.init(&ctx).unwrap();
        assert!(
            plugin.capabilities().is_empty(),
            "capabilities should be empty when ffprobe is not callable"
        );
    }
}
