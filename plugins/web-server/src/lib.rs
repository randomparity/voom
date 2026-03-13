//! Web Server Plugin for VOOM.
//!
//! Provides:
//! - REST API (JSON) for files, jobs, plans, plugins, stats, policy validate/format
//! - Web dashboard with Tera templates, htmx, and Alpine.js
//! - SSE for live job/scan progress updates

pub mod api;
pub mod error;
pub mod middleware;
pub mod router;
pub mod server;
pub mod sse;
pub mod state;
pub mod templates;
pub mod views;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_kernel::{Plugin, PluginContext};

/// The web server plugin.
pub struct WebServerPlugin {
    capabilities: Vec<Capability>,
}

impl WebServerPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::ServeHttp],
        }
    }
}

impl Default for WebServerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for WebServerPlugin {
    fn name(&self) -> &str {
        "web-server"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, _event_type: &str) -> bool {
        false // Web server doesn't handle events — it only reads data
    }

    fn on_event(&self, _event: &Event) -> Result<Option<EventResult>> {
        Ok(None)
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        tracing::info!("Web server plugin initialized");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = WebServerPlugin::new();
        assert_eq!(plugin.name(), "web-server");
        assert!(!plugin.version().is_empty());
        assert_eq!(plugin.capabilities().len(), 1);
        assert_eq!(plugin.capabilities()[0].kind(), "serve_http");
    }

    #[test]
    fn test_plugin_handles_no_events() {
        let plugin = WebServerPlugin::new();
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("job.started"));
    }

    #[test]
    fn test_plugin_on_event_returns_none() {
        let plugin = WebServerPlugin::new();
        let event = Event::JobStarted(voom_domain::events::JobStartedEvent {
            job_id: "test-1".into(),
            description: "test".into(),
        });
        assert!(plugin.on_event(&event).unwrap().is_none());
    }
}
