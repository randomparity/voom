//! Web Server Plugin for VOOM.
//!
//! Provides:
//! - REST API (JSON) for files, jobs, plans, plugins, stats, policy validate/format
//! - Web dashboard with Tera templates, htmx, and Alpine.js
//! - SSE for live job/scan progress updates

pub mod api;
pub mod errors;
pub mod middleware;
pub mod router;
pub mod server;
pub mod sse;
pub mod state;
pub mod templates;
pub mod views;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_kernel::{Plugin, PluginContext};

/// The web server plugin.
///
/// This Plugin impl handles no events and performs no work during `init()`.
/// It exists so the plugin registry can list and discover the web server as a
/// registered plugin with the `ServeHttp` capability. The actual web server
/// lifecycle (binding, serving, shutdown) is managed separately by the
/// `voom serve` CLI command via `server::run()`.
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

    /// No-op: the web server lifecycle is managed by the `serve` CLI command
    /// via `start_server()`, not through the plugin init/event system.
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::Event;

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
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::JOB_STARTED));
    }

    #[test]
    fn test_plugin_on_event_returns_none() {
        let plugin = WebServerPlugin::new();
        let event = Event::JobStarted(voom_domain::events::JobStartedEvent::new(
            uuid::Uuid::new_v4(),
            "test",
        ));
        assert!(plugin.on_event(&event).unwrap().is_none());
    }
}
