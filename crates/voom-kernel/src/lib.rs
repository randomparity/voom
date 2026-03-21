//! VOOM kernel: event bus, plugin registry, capability routing, and plugin loaders.

#![allow(clippy::missing_errors_doc)]

pub mod bus;
pub mod host;
pub mod loader;
pub mod manifest;
pub mod registry;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};

/// Universal plugin interface. All native plugins implement this.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    /// Returns `true` if this plugin wants to receive events of the given type.
    ///
    /// Use the constants on [`Event`] (e.g. `Event::FILE_DISCOVERED`,
    /// `Event::PLAN_CREATED`) rather than string literals to get compile-time
    /// typo protection. The constants are defined in `voom_domain::events`.
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
    shutdown_called: AtomicBool,
}

impl Kernel {
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: registry::PluginRegistry::new(),
            bus: bus::EventBus::new(),
            shutdown_called: AtomicBool::new(false),
        }
    }

    /// Register a native plugin, subscribing it to events it handles.
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>, priority: i32) {
        let name = plugin.name().to_string();
        self.registry.register(plugin.clone());
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin registered");
    }

    /// Initialize a plugin via `init()`, then register it with the given priority.
    ///
    /// This is the safe-by-default path that ensures every plugin is initialized
    /// before being registered. Prefer this over manually calling `init` + [`register_plugin`](Self::register_plugin).
    ///
    /// Accepts `Arc<dyn Plugin>` for consistency with [`register_plugin`](Self::register_plugin). The caller
    /// must pass a freshly created `Arc` (refcount == 1) so that `Arc::get_mut` can
    /// obtain the `&mut` reference needed to call `Plugin::init`.
    pub fn init_and_register(
        &mut self,
        mut plugin: Arc<dyn Plugin>,
        priority: i32,
        ctx: &PluginContext,
    ) -> Result<()> {
        let name = plugin.name().to_string();
        let plugin_mut =
            Arc::get_mut(&mut plugin).ok_or_else(|| voom_domain::errors::VoomError::Plugin {
                plugin: name.clone(),
                message: "init_and_register requires exclusive Arc ownership (refcount must be 1)"
                    .into(),
            })?;
        plugin_mut
            .init(ctx)
            .map_err(|e| voom_domain::errors::VoomError::Plugin {
                plugin: name.clone(),
                message: format!("init failed: {e}"),
            })?;
        self.registry.register(plugin.clone());
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin initialized and registered");
        Ok(())
    }

    /// Dispatch an event through the bus to all matching subscribers.
    pub fn dispatch(&self, event: Event) -> Vec<EventResult> {
        self.bus.publish(event)
    }

    /// Gracefully shut down all plugins in reverse priority order.
    ///
    /// Safe to call multiple times — only the first call runs shutdown logic.
    pub fn shutdown(&self) {
        if self
            .shutdown_called
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let subscribers = self.bus.subscribers_ordered();
        for (name, plugin) in subscribers.iter().rev() {
            if let Err(e) = plugin.shutdown() {
                tracing::error!(plugin = %name, error = %e, "plugin shutdown failed");
            } else {
                tracing::debug!(plugin = %name, "plugin shut down");
            }
        }
        tracing::info!("kernel shutdown complete");
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    struct LifecyclePlugin {
        init_called: Arc<AtomicBool>,
        shutdown_called: Arc<AtomicBool>,
    }

    impl Plugin for LifecyclePlugin {
        fn name(&self) -> &str {
            "lifecycle-test"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, _: &str) -> bool {
            false
        }
        fn on_event(&self, _: &Event) -> Result<Option<EventResult>> {
            Ok(None)
        }
        fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
            self.init_called.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn shutdown(&self) -> Result<()> {
            self.shutdown_called.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn test_init_and_register_calls_init() {
        let init_called = Arc::new(AtomicBool::new(false));
        let shutdown_called = Arc::new(AtomicBool::new(false));

        let plugin = Arc::new(LifecyclePlugin {
            init_called: init_called.clone(),
            shutdown_called: shutdown_called.clone(),
        });

        let ctx = PluginContext {
            config: serde_json::json!({}),
            data_dir: PathBuf::from("/tmp"),
        };

        let mut kernel = Kernel::new();
        kernel.init_and_register(plugin, 50, &ctx).unwrap();

        assert!(init_called.load(Ordering::SeqCst));
        assert_eq!(kernel.registry.len(), 1);
        assert_eq!(kernel.bus.subscriber_count(), 1);
    }

    #[test]
    fn test_drop_calls_shutdown() {
        let shutdown_called = Arc::new(AtomicBool::new(false));

        {
            let plugin = Arc::new(LifecyclePlugin {
                init_called: Arc::new(AtomicBool::new(false)),
                shutdown_called: shutdown_called.clone(),
            });

            let ctx = PluginContext {
                config: serde_json::json!({}),
                data_dir: PathBuf::from("/tmp"),
            };

            let mut kernel = Kernel::new();
            kernel.init_and_register(plugin, 50, &ctx).unwrap();
            // kernel dropped here
        }

        assert!(shutdown_called.load(Ordering::SeqCst));
    }

    #[test]
    fn test_double_shutdown_is_safe() {
        let shutdown_called = Arc::new(AtomicBool::new(false));

        let plugin = Arc::new(LifecyclePlugin {
            init_called: Arc::new(AtomicBool::new(false)),
            shutdown_called: shutdown_called.clone(),
        });

        let ctx = PluginContext {
            config: serde_json::json!({}),
            data_dir: PathBuf::from("/tmp"),
        };

        let mut kernel = Kernel::new();
        kernel.init_and_register(plugin, 50, &ctx).unwrap();

        kernel.shutdown();
        assert!(shutdown_called.load(Ordering::SeqCst));

        // Second call should be a no-op (no panic).
        kernel.shutdown();
    }
}
