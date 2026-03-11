pub mod bus;
pub mod capabilities;
pub mod loader;
pub mod manifest;
pub mod registry;

use std::path::PathBuf;
use std::sync::Arc;
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};

/// Universal plugin interface. All native plugins implement this.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    fn handles(&self, event_type: &str) -> bool;
    fn on_event(&self, event: &Event) -> Result<Option<EventResult>>;

    /// Called once after the plugin is loaded.
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        Ok(())
    }

    /// Called on application shutdown.
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// Configuration and resources provided to a plugin during initialization.
pub struct PluginContext {
    pub config: serde_json::Value,
    pub data_dir: PathBuf,
}

/// The kernel that manages plugins and event dispatch.
pub struct Kernel {
    pub registry: registry::PluginRegistry,
    pub bus: bus::EventBus,
}

impl Kernel {
    pub fn new() -> Self {
        Self {
            registry: registry::PluginRegistry::new(),
            bus: bus::EventBus::new(),
        }
    }

    /// Register a native plugin, subscribing it to events it handles.
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>, priority: i32) {
        let name = plugin.name().to_string();
        self.registry.register(plugin.clone());
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin registered");
    }

    /// Dispatch an event through the bus to all matching subscribers.
    pub async fn dispatch(&self, event: Event) -> Vec<EventResult> {
        self.bus.publish(event).await
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}
