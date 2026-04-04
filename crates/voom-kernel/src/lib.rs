//! VOOM kernel: event bus, plugin registry, capability routing, and plugin loaders.

pub mod bus;
pub mod errors;
#[cfg(feature = "wasm")]
pub mod host;
pub mod loader;
pub mod manifest;
pub mod registry;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};

/// Implements `description`, `author`, `license`, and `homepage` from Cargo.toml metadata.
///
/// Place inside a `Plugin` impl block to fill in the four metadata methods
/// using compile-time `env!()` macros from the plugin crate's Cargo.toml.
#[macro_export]
macro_rules! plugin_cargo_metadata {
    () => {
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
    };
}

/// Universal plugin interface. All native plugins implement this.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;

    /// Human-readable description of what this plugin does.
    fn description(&self) -> &str {
        ""
    }

    /// Plugin author(s).
    fn author(&self) -> &str {
        ""
    }

    /// License identifier (e.g., "MIT", "Apache-2.0").
    fn license(&self) -> &str {
        ""
    }

    /// Project homepage or repository URL.
    fn homepage(&self) -> &str {
        ""
    }

    fn capabilities(&self) -> &[Capability];
    /// Returns `true` if this plugin wants to receive events of the given type.
    ///
    /// Use the constants on [`Event`] (e.g. `Event::FILE_DISCOVERED`,
    /// `Event::PLAN_CREATED`) rather than string literals to get compile-time
    /// typo protection. The constants are defined in `voom_domain::events`.
    ///
    /// Default: returns `false` for all event types. Plugins that participate
    /// in event-driven coordination must override this.
    fn handles(&self, _event_type: &str) -> bool {
        false
    }

    /// Process an incoming event. Only called for event types where
    /// [`handles`](Self::handles) returns `true`.
    ///
    /// Default: returns `Ok(None)` (no result produced).
    fn on_event(&self, _event: &Event) -> Result<Option<EventResult>> {
        Ok(None)
    }

    /// Called once after the plugin is loaded.
    ///
    /// Returns a list of events to dispatch through the bus after the plugin
    /// is registered. This allows plugins to emit initial state (e.g. detected
    /// tools) that other already-registered plugins can observe.
    fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
        Ok(vec![])
    }

    /// Called on application shutdown.
    fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

/// Configuration and resources provided to a plugin during initialization.
///
/// Plugin config is stored as JSON internally for WASM compatibility.
/// Use [`parse_config`](Self::parse_config) for typed access.
pub struct PluginContext {
    config: serde_json::Value,
    pub data_dir: PathBuf,
    resources: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl PluginContext {
    #[must_use]
    pub fn new(config: serde_json::Value, data_dir: PathBuf) -> Self {
        Self {
            config,
            data_dir,
            resources: HashMap::new(),
        }
    }

    /// Register a shared resource that plugins can retrieve during init.
    pub fn register_resource<T: Send + Sync + 'static>(&mut self, resource: Arc<T>) {
        self.resources.insert(TypeId::of::<T>(), resource);
    }

    /// Retrieve a shared resource by type.
    pub fn resource<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.resources
            .get(&TypeId::of::<T>())
            .and_then(|r| r.clone().downcast::<T>().ok())
    }

    /// Deserialize the config into a typed struct.
    ///
    /// # Errors
    /// Returns `VoomError::Plugin` if the config JSON cannot be deserialized
    /// into `T`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use serde::Deserialize;
    /// use voom_kernel::PluginContext;
    ///
    /// #[derive(Deserialize)]
    /// struct MyConfig {
    ///     threshold: u32,
    /// }
    ///
    /// let ctx = PluginContext::new(
    ///     serde_json::json!({"threshold": 42}),
    ///     PathBuf::from("/tmp/plugin-data"),
    /// );
    /// let config: MyConfig = ctx.parse_config().unwrap();
    /// assert_eq!(config.threshold, 42);
    /// ```
    pub fn parse_config<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.config.clone()).map_err(|e| {
            voom_domain::errors::VoomError::Plugin {
                plugin: "config".into(),
                message: format!("config deserialization failed: {e}"),
            }
        })
    }
}

/// The kernel that manages plugins and event dispatch.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use voom_domain::capabilities::Capability;
/// use voom_domain::events::{Event, EventResult, FileDiscoveredEvent};
/// use voom_kernel::{Kernel, Plugin};
///
/// struct EchoPlugin;
///
/// impl Plugin for EchoPlugin {
///     fn name(&self) -> &str { "echo" }
///     fn version(&self) -> &str { "0.1.0" }
///     fn capabilities(&self) -> &[Capability] { &[] }
///     fn handles(&self, event_type: &str) -> bool {
///         event_type == Event::FILE_DISCOVERED
///     }
///     fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
///         Ok(Some(EventResult::new("echo")))
///     }
/// }
///
/// let mut kernel = Kernel::new();
/// kernel.register_plugin(Arc::new(EchoPlugin), 100).unwrap();
/// assert_eq!(kernel.subscriber_count(), 1);
///
/// let event = Event::FileDiscovered(FileDiscoveredEvent::new(
///     "/test.mkv".into(), 42, None,
/// ));
/// let results = kernel.dispatch(event);
/// assert_eq!(results.len(), 1);
/// assert_eq!(results[0].plugin_name, "echo");
/// ```
pub struct Kernel {
    pub registry: registry::PluginRegistry,
    pub(crate) bus: bus::EventBus,
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
    ///
    /// Returns an error if a plugin with the same name is already registered.
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>, priority: i32) -> Result<()> {
        let name = plugin.name().to_string();
        self.registry.register(plugin.clone())?;
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin registered");
        Ok(())
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
        if self.registry.contains(&name) {
            return Err(voom_domain::errors::VoomError::Plugin {
                plugin: name,
                message: "a plugin with this name is already registered".into(),
            });
        }
        let plugin_mut =
            Arc::get_mut(&mut plugin).ok_or_else(|| voom_domain::errors::VoomError::Plugin {
                plugin: name.clone(),
                message: "init_and_register requires exclusive Arc ownership (refcount must be 1)"
                    .into(),
            })?;
        let init_events =
            plugin_mut
                .init(ctx)
                .map_err(|e| voom_domain::errors::VoomError::Plugin {
                    plugin: name.clone(),
                    message: format!("init failed: {e}"),
                })?;
        self.registry.register(plugin.clone())?;
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin initialized and registered");

        for event in init_events {
            self.dispatch(event);
        }

        Ok(())
    }

    /// Dispatch an event through the bus to all matching subscribers.
    pub fn dispatch(&self, event: Event) -> Vec<EventResult> {
        let event_type = event.event_type().to_string();
        let _span = tracing::debug_span!("dispatch", event = %event_type).entered();
        self.bus.publish(event)
    }

    /// Returns the number of subscribers registered on the event bus.
    pub fn subscriber_count(&self) -> usize {
        self.bus.subscriber_count()
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
        fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
            self.init_called.store(true, Ordering::SeqCst);
            Ok(vec![])
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

        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        let mut kernel = Kernel::new();
        kernel.init_and_register(plugin, 50, &ctx).unwrap();

        assert!(init_called.load(Ordering::SeqCst));
        assert_eq!(kernel.registry.len(), 1);
        assert_eq!(kernel.subscriber_count(), 1);
    }

    #[test]
    fn test_drop_calls_shutdown() {
        let shutdown_called = Arc::new(AtomicBool::new(false));

        {
            let plugin = Arc::new(LifecyclePlugin {
                init_called: Arc::new(AtomicBool::new(false)),
                shutdown_called: shutdown_called.clone(),
            });

            let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

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

        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        let mut kernel = Kernel::new();
        kernel.init_and_register(plugin, 50, &ctx).unwrap();

        kernel.shutdown();
        assert!(shutdown_called.load(Ordering::SeqCst));

        // Second call should be a no-op (no panic).
        kernel.shutdown();
    }

    /// Plugin that emits an event from init() and subscribes to it.
    struct InitEventEmitter;

    impl Plugin for InitEventEmitter {
        fn name(&self) -> &str {
            "init-emitter"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
            Ok(vec![Event::ToolDetected(
                voom_domain::events::ToolDetectedEvent::new(
                    "test-tool",
                    "1.0.0",
                    "/usr/bin/test-tool".into(),
                ),
            )])
        }
    }

    /// Plugin that records whether it received a ToolDetected event.
    struct EventCapture {
        received: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Plugin for EventCapture {
        fn name(&self) -> &str {
            "event-capture"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, event_type: &str) -> bool {
            event_type == Event::TOOL_DETECTED
        }
        fn on_event(&self, event: &Event) -> Result<Option<voom_domain::events::EventResult>> {
            if let Event::ToolDetected(e) = event {
                self.received
                    .lock()
                    .expect("lock poisoned")
                    .push(e.tool_name.clone());
            }
            Ok(None)
        }
    }

    #[test]
    fn test_init_events_dispatched_after_registration() {
        let received = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

        let mut kernel = Kernel::new();
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        // Register the capture plugin first (lower priority = earlier registration)
        let capture = Arc::new(EventCapture {
            received: received.clone(),
        });
        kernel.register_plugin(capture, 10).unwrap();

        // Now init_and_register the emitter — its init events should reach the capture plugin
        let emitter = Arc::new(InitEventEmitter);
        kernel.init_and_register(emitter, 20, &ctx).unwrap();

        let captured = received.lock().expect("lock poisoned");
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0], "test-tool");
    }

    #[test]
    fn test_plugin_context_resource_map() {
        let mut ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        let value = Arc::new(42_u64);
        ctx.register_resource(value);

        let retrieved = ctx.resource::<u64>();
        assert_eq!(retrieved.as_deref(), Some(&42));
    }

    #[test]
    fn test_plugin_context_resource_missing_type() {
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        let result = ctx.resource::<String>();
        assert!(result.is_none());
    }

    #[test]
    fn test_plugin_context_resource_overwrite() {
        let mut ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        ctx.register_resource(Arc::new(1_u32));
        ctx.register_resource(Arc::new(2_u32));
        assert_eq!(ctx.resource::<u32>().as_deref(), Some(&2));
    }

    #[test]
    fn test_duplicate_plugin_registration_rejected() {
        let mut kernel = Kernel::new();

        let p1 = Arc::new(LifecyclePlugin {
            init_called: Arc::new(AtomicBool::new(false)),
            shutdown_called: Arc::new(AtomicBool::new(false)),
        });
        let p2 = Arc::new(LifecyclePlugin {
            init_called: Arc::new(AtomicBool::new(false)),
            shutdown_called: Arc::new(AtomicBool::new(false)),
        });

        kernel.register_plugin(p1, 10).unwrap();
        let err = kernel.register_plugin(p2, 20).unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "expected 'already registered' error, got: {err}"
        );

        // Original plugin still present, no duplicate in bus.
        assert_eq!(kernel.registry.len(), 1);
        assert_eq!(kernel.subscriber_count(), 1);
    }

    #[test]
    fn test_duplicate_init_and_register_rejected_without_calling_init() {
        let mut kernel = Kernel::new();
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        let p1 = Arc::new(LifecyclePlugin {
            init_called: Arc::new(AtomicBool::new(false)),
            shutdown_called: Arc::new(AtomicBool::new(false)),
        });
        let p2_init_called = Arc::new(AtomicBool::new(false));
        let p2 = Arc::new(LifecyclePlugin {
            init_called: p2_init_called.clone(),
            shutdown_called: Arc::new(AtomicBool::new(false)),
        });

        kernel.init_and_register(p1, 10, &ctx).unwrap();
        let err = kernel.init_and_register(p2, 20, &ctx).unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "expected 'already registered' error, got: {err}"
        );
        assert!(
            !p2_init_called.load(Ordering::SeqCst),
            "duplicate plugin must not have init() called"
        );
        assert_eq!(kernel.registry.len(), 1);
        assert_eq!(kernel.subscriber_count(), 1);
    }
}
