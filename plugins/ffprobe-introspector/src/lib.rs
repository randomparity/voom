//! `FFprobe` introspection plugin: media file analysis via ffprobe JSON output.

#![allow(clippy::missing_errors_doc)]

pub mod ffprobe;
pub mod parser;

use std::time::Duration;

use serde::Deserialize;
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult, FileIntrospectedEvent};
use voom_kernel::{Plugin, PluginContext};

/// Typed configuration for the ffprobe introspector plugin.
#[derive(Debug, Default, Deserialize)]
struct FfprobeConfig {
    /// Custom path to the ffprobe binary.
    ffprobe_path: Option<String>,
}

/// `FFprobe` introspector plugin: extracts media metadata using ffprobe.
///
/// Listens for `FileDiscovered` events, runs ffprobe on each file,
/// and emits `FileIntrospected` events with complete `MediaFile` data.
pub struct FfprobeIntrospectorPlugin {
    capabilities: Vec<Capability>,
    ffprobe_path: String,
    timeout: Duration,
}

impl FfprobeIntrospectorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Introspect {
                formats: vec![
                    "mkv".into(),
                    "mp4".into(),
                    "avi".into(),
                    "webm".into(),
                    "flv".into(),
                    "wmv".into(),
                    "mov".into(),
                    "ts".into(),
                ],
            }],
            ffprobe_path: "ffprobe".into(),
            timeout: Duration::from_secs(60),
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

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::FILE_DISCOVERED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::FileDiscovered(e) => match self.introspect(&e.path, e.size, &e.content_hash) {
                Ok(introspected) => {
                    tracing::info!(
                        path = %e.path.display(),
                        tracks = introspected.file.tracks.len(),
                        "file introspected"
                    );
                    let mut result = EventResult::new(self.name());
                    result.produced_events = vec![Event::FileIntrospected(introspected)];
                    Ok(Some(result))
                }
                Err(err) => {
                    tracing::warn!(
                        path = %e.path.display(),
                        error = %err,
                        "failed to introspect file"
                    );
                    Err(err)
                }
            },
            _ => Ok(None),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<()> {
        let config = ctx.parse_config::<FfprobeConfig>();
        if let Some(path) = config.ffprobe_path {
            self.ffprobe_path = path;
        }
        tracing::info!(ffprobe = %self.ffprobe_path, "ffprobe introspector initialized");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert_eq!(plugin.name(), "ffprobe-introspector");
        assert!(!plugin.capabilities().is_empty());
        assert_eq!(plugin.capabilities()[0].kind(), "introspect");
    }

    #[test]
    fn test_handles_file_discovered() {
        let plugin = FfprobeIntrospectorPlugin::new();
        assert!(plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::FILE_INTROSPECTED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn test_custom_ffprobe_path() {
        let plugin = FfprobeIntrospectorPlugin::new().with_ffprobe_path("/usr/local/bin/ffprobe");
        assert_eq!(plugin.ffprobe_path(), "/usr/local/bin/ffprobe");
    }

    #[test]
    fn test_ignores_non_discovered_events() {
        let plugin = FfprobeIntrospectorPlugin::new();
        let event = Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
            "ffprobe",
            "6.1",
            std::path::PathBuf::from("/usr/bin/ffprobe"),
        ));
        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_none());
    }
}
