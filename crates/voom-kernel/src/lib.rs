//! VOOM kernel: event bus, plugin registry, capability routing, and plugin loaders.

pub mod bus;
pub mod errors;
#[cfg(feature = "wasm")]
pub mod host;
pub mod loader;
pub mod manifest;
pub mod registry;
pub mod stats_sink;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Stat-bucket label for a `Call` variant. Each variant has a stable
/// `call.<snake>` label that's safe to embed in low-cardinality metrics.
fn call_kind_str(call: &voom_domain::call::Call) -> &'static str {
    use voom_domain::call::Call;
    match call {
        Call::EvaluatePolicy { .. } => "call.evaluate_policy",
        Call::Orchestrate { .. } => "call.orchestrate",
        Call::ScanLibrary { .. } => "call.scan_library",
        // `Call` is `#[non_exhaustive]`; future variants land here until a
        // dedicated label is added.
        _ => "call.unknown",
    }
}

/// If the two Sharded capabilities have a key in common, return the colliding
/// key. Used by `check_capability_collisions` to surface the specific key
/// that conflicts (e.g. scheme `"file"`) in the error message.
fn sharded_collision_key(
    a: &voom_domain::capabilities::Capability,
    b: &voom_domain::capabilities::Capability,
) -> Option<String> {
    use voom_domain::capabilities::Capability;
    match (a, b) {
        (Capability::Discover { schemes: a_s }, Capability::Discover { schemes: b_s }) => {
            a_s.iter().find(|s| b_s.contains(*s)).cloned()
        }
        (
            Capability::EnrichMetadata { source: a_src },
            Capability::EnrichMetadata { source: b_src },
        ) => (a_src == b_src).then(|| a_src.clone()),
        _ => None,
    }
}

/// Universal plugin interface. All native plugins implement this.
///
/// `Plugin: Any + 'static` allows `Registry::get_typed::<P>` to recover the
/// concrete plugin type by TypeId match. The `'static` bound is implied by
/// `Any` and is satisfied by every plugin in this workspace.
pub trait Plugin: Any + Send + Sync {
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

    /// Handle a unary or streaming Call dispatched via
    /// `Kernel::dispatch_to_capability`. Default impl returns an error so a
    /// plugin that does NOT claim any capability that expects calls compiles
    /// without override but fails cleanly if invoked.
    ///
    /// Plugins that expose Call-handling capabilities (e.g. EvaluatePolicy,
    /// OrchestratePhases, Discover) override this.
    fn on_call(&self, _call: &voom_domain::call::Call) -> Result<voom_domain::call::CallResponse> {
        Err(voom_domain::errors::VoomError::plugin(
            self.name(),
            "plugin does not handle calls (no on_call override)",
        ))
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
    /// Returns an error if a plugin with the same name is already registered,
    /// or if the new plugin's capabilities collide with an existing claim
    /// (Exclusive: kind already taken; Sharded: a key overlaps).
    pub fn register_plugin(&mut self, plugin: Arc<dyn Plugin>, priority: i32) -> Result<()> {
        self.verify_registration_invariants(&plugin)?;
        let name = plugin.name().to_string();
        self.registry.register(plugin.clone())?;
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin registered");
        Ok(())
    }

    /// Pre-registration invariant check called by every registration entry
    /// point before any plugin-side `init()` side effects can run on a doomed
    /// plugin (rev-3). Today this only enforces capability-collision rules;
    /// future invariants (manifest sanity checks, etc.) can plug in here
    /// without re-wiring every entry point.
    fn verify_registration_invariants(&self, new_plugin: &Arc<dyn Plugin>) -> Result<()> {
        self.check_capability_collisions(new_plugin)
    }

    fn check_capability_collisions(&self, new_plugin: &Arc<dyn Plugin>) -> Result<()> {
        use voom_domain::capability_resolution::CapabilityResolution;
        let new_name = new_plugin.name().to_string();

        for new_cap in new_plugin.capabilities() {
            for (existing_name, existing_plugin) in self.registry.iter_all() {
                if existing_name == new_name {
                    continue;
                }
                for existing_cap in existing_plugin.capabilities() {
                    if new_cap.kind() != existing_cap.kind() {
                        continue;
                    }
                    match new_cap.resolution() {
                        CapabilityResolution::Exclusive => {
                            return Err(voom_domain::errors::VoomError::plugin(
                                &new_name,
                                format!(
                                    "capability conflict: {} already claimed by '{}'",
                                    new_cap.kind(),
                                    existing_name,
                                ),
                            ));
                        }
                        CapabilityResolution::Sharded => {
                            if let Some(key) = sharded_collision_key(new_cap, existing_cap) {
                                return Err(voom_domain::errors::VoomError::plugin(
                                    &new_name,
                                    format!(
                                        "capability conflict: {} key '{}' already claimed by '{}'",
                                        new_cap.kind(),
                                        key,
                                        existing_name,
                                    ),
                                ));
                            }
                        }
                        CapabilityResolution::Competing => {}
                    }
                }
            }
        }
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
            return Err(voom_domain::errors::VoomError::plugin(
                name,
                "a plugin with this name is already registered",
            ));
        }
        // rev-3: collision check BEFORE init() side effects.
        self.verify_registration_invariants(&plugin)?;
        let plugin_mut = Arc::get_mut(&mut plugin).ok_or_else(|| {
            voom_domain::errors::VoomError::plugin(
                name.clone(),
                "init requires exclusive Arc ownership (refcount must be 1)",
            )
        })?;
        let init_events = plugin_mut.init(ctx).map_err(|e| {
            voom_domain::errors::VoomError::plugin(name.clone(), format!("init failed: {e}"))
        })?;
        self.finish_registration(plugin, priority, &name, init_events)
    }

    /// Initialize a typed plugin, register it, and return an `Arc<P>` handle.
    ///
    /// Use this instead of [`init_and_register`](Self::init_and_register) when the caller needs a
    /// typed `Arc<P>` for later use (e.g. the capability collector, whose
    /// `snapshot()` method is called after bootstrap). Takes `P` by value so
    /// the kernel constructs the `Arc` itself — guaranteeing the refcount-1
    /// invariant that `Arc::get_mut` requires during `init()`.
    pub fn init_and_register_shared<P: Plugin + 'static>(
        &mut self,
        plugin: P,
        priority: i32,
        ctx: &PluginContext,
    ) -> Result<Arc<P>> {
        let mut arc: Arc<P> = Arc::new(plugin);
        let name = arc.name().to_string();
        if self.registry.contains(&name) {
            return Err(voom_domain::errors::VoomError::plugin(
                name,
                "a plugin with this name is already registered",
            ));
        }
        // rev-3: collision check BEFORE init() side effects. Cloning the typed
        // Arc bumps the refcount to 2 just long enough to coerce it to a
        // `&Arc<dyn Plugin>` for the invariant check; the clone is dropped
        // before `Arc::get_mut` (which requires refcount-1) is called.
        {
            let arc_dyn: Arc<dyn Plugin> = arc.clone();
            self.verify_registration_invariants(&arc_dyn)?;
        }
        let plugin_mut = Arc::get_mut(&mut arc).ok_or_else(|| {
            voom_domain::errors::VoomError::plugin(
                name.clone(),
                "internal error: Arc refcount > 1 before init (kernel-constructed Arc should be unique)",
            )
        })?;
        let init_events = plugin_mut.init(ctx).map_err(|e| {
            voom_domain::errors::VoomError::plugin(name.clone(), format!("init failed: {e}"))
        })?;
        self.finish_registration(arc.clone(), priority, &name, init_events)?;
        Ok(arc)
    }

    /// Shared tail of both init-and-register paths: insert into the registry,
    /// subscribe on the bus, and dispatch init events. Kept separate from the
    /// init step so each caller can preserve the `Arc::get_mut` refcount-1
    /// invariant on its own `Arc`.
    fn finish_registration(
        &mut self,
        plugin: Arc<dyn Plugin>,
        priority: i32,
        name: &str,
        init_events: Vec<Event>,
    ) -> Result<()> {
        self.registry.register(plugin.clone())?;
        self.bus.subscribe_plugin(plugin, priority);
        tracing::info!(plugin = %name, "plugin initialized and registered");
        for event in init_events {
            self.dispatch(event);
        }
        Ok(())
    }

    /// Install a `StatsSink` that the event bus will forward every plugin
    /// invocation record to. Idempotent; calling repeatedly replaces the
    /// previous sink. Intended for one-shot wiring at bootstrap.
    pub fn set_stats_sink(&self, sink: Arc<dyn stats_sink::StatsSink>) {
        self.bus.set_stats_sink(sink);
    }

    /// Dispatch a unary or streaming `Call` to the plugin that owns the
    /// matching capability, recording its outcome (Ok / Err / Panic) through
    /// the installed `StatsSink`.
    ///
    /// Resolution rules:
    /// - `CapabilityQuery::Exclusive` and `::Sharded` resolve to exactly one
    ///   plugin (enforced at registration; see `verify_registration_invariants`).
    /// - `CapabilityQuery::Competing` matches any plugin in the priority-ordered
    ///   set; today returns the first hit (priority sorting can be added later).
    /// - No match → `VoomError::Plugin` naming the query.
    pub fn dispatch_to_capability(
        &self,
        query: voom_domain::capabilities::CapabilityQuery,
        call: voom_domain::call::Call,
    ) -> Result<voom_domain::call::CallResponse> {
        let plugin = self.resolve_capability(&query)?;
        let plugin_name = plugin.name().to_string();
        let call_kind = call_kind_str(&call);

        let started_at = chrono::Utc::now();
        let timer = std::time::Instant::now();
        let handler_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            plugin.on_call(&call)
        }));
        let duration_ms = u64::try_from(timer.elapsed().as_millis()).unwrap_or(u64::MAX);

        let outcome = match &handler_result {
            Ok(Ok(_)) => voom_domain::plugin_stats::PluginInvocationOutcome::Ok,
            Ok(Err(e)) => voom_domain::plugin_stats::PluginInvocationOutcome::Err {
                category: crate::bus::error_category(e),
            },
            Err(_) => voom_domain::plugin_stats::PluginInvocationOutcome::Panic,
        };
        self.bus
            .stats_sink_snapshot()
            .record(voom_domain::plugin_stats::PluginStatRecord::new(
                plugin_name.clone(),
                call_kind,
                started_at,
                duration_ms,
                outcome,
            ));

        match handler_result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(voom_domain::errors::VoomError::plugin(
                plugin_name,
                "panic during dispatch_to_capability",
            )),
        }
    }

    fn resolve_capability(
        &self,
        query: &voom_domain::capabilities::CapabilityQuery,
    ) -> Result<Arc<dyn Plugin>> {
        let mut matches: Vec<Arc<dyn Plugin>> = Vec::new();
        for (_name, plugin) in self.registry.iter_all() {
            if plugin.capabilities().iter().any(|c| c.matches_query(query)) {
                matches.push(plugin);
            }
        }
        if matches.is_empty() {
            return Err(voom_domain::errors::VoomError::plugin(
                format!("{query:?}"),
                format!("no handler for capability {query:?}"),
            ));
        }
        Ok(matches.into_iter().next().expect("non-empty checked above"))
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
            let shutdown_result = plugin.shutdown();
            match shutdown_result {
                Err(e) => {
                    tracing::error!(plugin = %name, error = %e, "plugin shutdown failed");
                }
                _ => {
                    tracing::debug!(plugin = %name, "plugin shut down");
                }
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
    use parking_lot::Mutex;
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
    fn test_init_and_register_shared_returns_typed_handle_and_calls_init() {
        let init_called = Arc::new(AtomicBool::new(false));
        let shutdown_called = Arc::new(AtomicBool::new(false));

        let plugin = LifecyclePlugin {
            init_called: init_called.clone(),
            shutdown_called: shutdown_called.clone(),
        };

        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        let mut kernel = Kernel::new();
        let handle: Arc<LifecyclePlugin> =
            kernel.init_and_register_shared(plugin, 50, &ctx).unwrap();

        assert!(init_called.load(Ordering::SeqCst));
        assert_eq!(kernel.registry.len(), 1);
        assert_eq!(kernel.subscriber_count(), 1);
        // The typed handle shares state with the kernel-owned Arc.
        assert!(handle.init_called.load(Ordering::SeqCst));
    }

    #[test]
    fn test_init_and_register_shared_rejects_duplicate_name() {
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        let mut kernel = Kernel::new();
        kernel
            .init_and_register_shared(
                LifecyclePlugin {
                    init_called: Arc::new(AtomicBool::new(false)),
                    shutdown_called: Arc::new(AtomicBool::new(false)),
                },
                50,
                &ctx,
            )
            .unwrap();
        let err = match kernel.init_and_register_shared(
            LifecyclePlugin {
                init_called: Arc::new(AtomicBool::new(false)),
                shutdown_called: Arc::new(AtomicBool::new(false)),
            },
            60,
            &ctx,
        ) {
            Err(e) => e,
            Ok(_) => panic!("expected duplicate registration to fail"),
        };
        assert!(
            format!("{err}").contains("already registered"),
            "expected already-registered error, got: {err}"
        );
    }

    #[test]
    fn test_drop_calls_shutdown_shared_with_retained_handle() {
        let shutdown_called = Arc::new(AtomicBool::new(false));
        let plugin = LifecyclePlugin {
            init_called: Arc::new(AtomicBool::new(false)),
            shutdown_called: shutdown_called.clone(),
        };
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));

        // Callers of init_and_register_shared typically retain the typed Arc
        // past kernel drop (e.g. BootstrapResult.collector). Verify shutdown
        // still fires exactly once during kernel drop, with no use-after-free.
        let handle: Arc<LifecyclePlugin>;
        {
            let mut kernel = Kernel::new();
            handle = kernel.init_and_register_shared(plugin, 50, &ctx).unwrap();
            assert!(!shutdown_called.load(Ordering::SeqCst));
            // kernel dropped here, while `handle` is still live
        }
        assert!(shutdown_called.load(Ordering::SeqCst));
        // `handle` is still valid here — accessing it must not panic.
        assert_eq!(handle.name(), "lifecycle-test");
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
        received: Arc<Mutex<Vec<String>>>,
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
                self.received.lock().push(e.tool_name.clone());
            }
            Ok(None)
        }
    }

    #[test]
    fn test_init_events_dispatched_after_registration() {
        let received = Arc::new(Mutex::new(Vec::<String>::new()));

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

        let captured = received.lock();
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

    // rev-3: capability collisions must be enforced at every registration
    // entry point, and BEFORE init() side effects can fire.

    #[test]
    fn register_rejects_exclusive_collision() {
        struct A;
        struct B;
        impl Plugin for A {
            fn name(&self) -> &str { "a" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
        }
        impl Plugin for B {
            fn name(&self) -> &str { "b" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
        }
        let mut kernel = Kernel::new();
        kernel.register_plugin(Arc::new(A), 10).unwrap();
        let err = kernel.register_plugin(Arc::new(B), 20).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name prior claimant: {err}");
    }

    #[test]
    fn init_and_register_rejects_exclusive_collision() {
        struct A;
        struct B;
        impl Plugin for A {
            fn name(&self) -> &str { "a" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
        }
        impl Plugin for B {
            fn name(&self) -> &str { "b" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
        }
        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        let mut kernel = Kernel::new();
        kernel.init_and_register(Arc::new(A), 10, &ctx).unwrap();
        let err = kernel.init_and_register(Arc::new(B), 20, &ctx).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name prior claimant: {err}");
    }

    #[test]
    fn init_and_register_shared_rejects_exclusive_collision() {
        #[derive(Debug)]
        struct ConflictingPlugin {
            init_called: Arc<AtomicBool>,
        }
        impl Plugin for ConflictingPlugin {
            fn name(&self) -> &str { "conflicting" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
            fn init(&mut self, _ctx: &PluginContext) -> Result<Vec<Event>> {
                self.init_called.store(true, Ordering::SeqCst);
                Ok(vec![])
            }
        }

        let ctx = PluginContext::new(serde_json::json!({}), PathBuf::from("/tmp"));
        let mut kernel = Kernel::new();

        struct FirstClaimant;
        impl Plugin for FirstClaimant {
            fn name(&self) -> &str { "first" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
        }
        kernel.register_plugin(Arc::new(FirstClaimant), 10).unwrap();

        let init_flag = Arc::new(AtomicBool::new(false));
        let err = kernel
            .init_and_register_shared(
                ConflictingPlugin { init_called: init_flag.clone() },
                20,
                &ctx,
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("first"),
            "error should name prior claimant: {err}"
        );
        assert!(
            !init_flag.load(Ordering::SeqCst),
            "collision must be detected BEFORE init() side effects fire"
        );
    }

    #[test]
    fn register_rejects_sharded_same_key() {
        use std::sync::LazyLock;
        struct D1;
        struct D2;
        impl Plugin for D1 {
            fn name(&self) -> &str { "d1" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Discover { schemes: vec!["file".into()] }]);
                &C
            }
        }
        impl Plugin for D2 {
            fn name(&self) -> &str { "d2" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Discover { schemes: vec!["file".into()] }]);
                &C
            }
        }
        let mut kernel = Kernel::new();
        kernel.register_plugin(Arc::new(D1), 10).unwrap();
        let err = kernel.register_plugin(Arc::new(D2), 20).unwrap_err();
        assert!(err.to_string().contains("file"), "error should name colliding scheme: {err}");
    }

    #[test]
    fn register_allows_sharded_disjoint_keys() {
        use std::sync::LazyLock;
        struct DFile;
        struct DS3;
        impl Plugin for DFile {
            fn name(&self) -> &str { "dfile" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Discover { schemes: vec!["file".into()] }]);
                &C
            }
        }
        impl Plugin for DS3 {
            fn name(&self) -> &str { "ds3" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Discover { schemes: vec!["s3".into()] }]);
                &C
            }
        }
        let mut kernel = Kernel::new();
        kernel.register_plugin(Arc::new(DFile), 10).unwrap();
        kernel.register_plugin(Arc::new(DS3), 20).expect("disjoint schemes allowed");
    }

    #[test]
    fn register_allows_competing() {
        use std::sync::LazyLock;
        struct I1;
        struct I2;
        impl Plugin for I1 {
            fn name(&self) -> &str { "i1" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Introspect { formats: vec!["mkv".into()] }]);
                &C
            }
        }
        impl Plugin for I2 {
            fn name(&self) -> &str { "i2" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] {
                static C: LazyLock<Vec<Capability>> =
                    LazyLock::new(|| vec![Capability::Introspect { formats: vec!["mp4".into()] }]);
                &C
            }
        }
        let mut kernel = Kernel::new();
        kernel.register_plugin(Arc::new(I1), 10).unwrap();
        kernel.register_plugin(Arc::new(I2), 20).expect("Competing allows co-claim");
    }

    fn minimal_policy_for_test() -> voom_domain::compiled::CompiledPolicy {
        use voom_domain::compiled::{
            CompiledConfig, CompiledMetadata, CompiledPolicy, ErrorStrategy,
        };
        CompiledPolicy::new(
            "demo".into(),
            CompiledMetadata::default(),
            CompiledConfig::new(vec![], vec![], ErrorStrategy::Abort, vec![], false),
            vec![],
            vec![],
            String::new(),
        )
    }

    #[test]
    fn dispatch_to_capability_routes_to_matching_plugin() {
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::capabilities::CapabilityQuery;
        use voom_domain::evaluation::EvaluationResult;
        use voom_domain::media::MediaFile;

        struct E;
        impl Plugin for E {
            fn name(&self) -> &str { "e" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
            fn on_call(&self, _: &Call) -> Result<CallResponse> {
                Ok(CallResponse::EvaluatePolicy(EvaluationResult::new(vec![])))
            }
        }

        let mut kernel = Kernel::new();
        kernel.register_plugin(Arc::new(E), 10).unwrap();
        let call = Call::EvaluatePolicy {
            policy: minimal_policy_for_test(),
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let response = kernel
            .dispatch_to_capability(
                CapabilityQuery::Exclusive { kind: "evaluate_policy".into() },
                call,
            )
            .expect("dispatch");
        match response {
            CallResponse::EvaluatePolicy(r) => assert_eq!(r.plans.len(), 0),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn dispatch_to_capability_no_handler_errors() {
        use voom_domain::call::Call;
        use voom_domain::capabilities::CapabilityQuery;
        use voom_domain::media::MediaFile;

        let kernel = Kernel::new();
        let call = Call::EvaluatePolicy {
            policy: minimal_policy_for_test(),
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let err = kernel
            .dispatch_to_capability(
                CapabilityQuery::Exclusive { kind: "evaluate_policy".into() },
                call,
            )
            .unwrap_err();
        assert!(err.to_string().contains("evaluate_policy"));
    }

    #[test]
    fn dispatch_to_capability_records_ok_stats() {
        use crate::stats_sink::StatsSink;
        use std::sync::Mutex;
        use voom_domain::call::{Call, CallResponse};
        use voom_domain::capabilities::CapabilityQuery;
        use voom_domain::evaluation::EvaluationResult;
        use voom_domain::media::MediaFile;
        use voom_domain::plugin_stats::{PluginInvocationOutcome, PluginStatRecord};

        #[derive(Default)]
        struct RecordingSink(Mutex<Vec<PluginStatRecord>>);
        impl StatsSink for RecordingSink {
            fn record(&self, r: PluginStatRecord) {
                self.0.lock().unwrap().push(r);
            }
        }

        struct E;
        impl Plugin for E {
            fn name(&self) -> &str { "e" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
            fn on_call(&self, _: &Call) -> Result<CallResponse> {
                Ok(CallResponse::EvaluatePolicy(EvaluationResult::new(vec![])))
            }
        }

        let sink = Arc::new(RecordingSink::default());
        let mut kernel = Kernel::new();
        kernel.set_stats_sink(sink.clone());
        kernel.register_plugin(Arc::new(E), 10).unwrap();
        let call = Call::EvaluatePolicy {
            policy: minimal_policy_for_test(),
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        kernel
            .dispatch_to_capability(
                CapabilityQuery::Exclusive { kind: "evaluate_policy".into() },
                call,
            )
            .unwrap();
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].plugin_id, "e");
        assert_eq!(records[0].event_type, "call.evaluate_policy");
        assert!(matches!(records[0].outcome, PluginInvocationOutcome::Ok));
    }

    #[test]
    fn dispatch_to_capability_records_panic_outcome() {
        use crate::stats_sink::StatsSink;
        use std::sync::Mutex;
        use voom_domain::call::Call;
        use voom_domain::capabilities::CapabilityQuery;
        use voom_domain::media::MediaFile;
        use voom_domain::plugin_stats::{PluginInvocationOutcome, PluginStatRecord};

        #[derive(Default)]
        struct RecordingSink(Mutex<Vec<PluginStatRecord>>);
        impl StatsSink for RecordingSink {
            fn record(&self, r: PluginStatRecord) {
                self.0.lock().unwrap().push(r);
            }
        }

        struct P;
        impl Plugin for P {
            fn name(&self) -> &str { "p" }
            fn version(&self) -> &str { "0.1.0" }
            fn capabilities(&self) -> &[Capability] { &[Capability::EvaluatePolicy] }
            fn on_call(&self, _: &Call) -> Result<voom_domain::call::CallResponse> {
                panic!("intentional");
            }
        }

        let sink = Arc::new(RecordingSink::default());
        let mut kernel = Kernel::new();
        kernel.set_stats_sink(sink.clone());
        kernel.register_plugin(Arc::new(P), 10).unwrap();
        let call = Call::EvaluatePolicy {
            policy: minimal_policy_for_test(),
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let result = kernel.dispatch_to_capability(
            CapabilityQuery::Exclusive { kind: "evaluate_policy".into() },
            call,
        );
        assert!(result.is_err());
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(records[0].outcome, PluginInvocationOutcome::Panic));
    }

    #[test]
    fn on_call_default_returns_error() {
        use std::path::PathBuf;
        use voom_domain::call::Call;
        use voom_domain::compiled::{CompiledConfig, CompiledMetadata, CompiledPolicy, ErrorStrategy};
        use voom_domain::media::MediaFile;

        struct MinimalPlugin;
        impl Plugin for MinimalPlugin {
            fn name(&self) -> &str {
                "minimal"
            }
            fn version(&self) -> &str {
                "0.1.0"
            }
            fn capabilities(&self) -> &[Capability] {
                &[]
            }
        }

        let plugin = MinimalPlugin;
        let policy = CompiledPolicy::new(
            "demo".into(),
            CompiledMetadata::default(),
            CompiledConfig::new(vec![], vec![], ErrorStrategy::Abort, vec![], false),
            vec![],
            vec![],
            String::new(),
        );
        let call = Call::EvaluatePolicy {
            policy,
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let err = plugin.on_call(&call).unwrap_err();
        assert!(err.to_string().contains("does not handle calls"));
    }
}

#[cfg(test)]
mod stats_sink_wiring_tests {
    use super::*;
    use crate::stats_sink::StatsSink;
    use std::sync::Mutex;
    use voom_domain::plugin_stats::PluginStatRecord;

    #[derive(Default)]
    struct CountingSink(Mutex<u64>);

    impl StatsSink for CountingSink {
        fn record(&self, _r: PluginStatRecord) {
            *self.0.lock().unwrap() += 1;
        }
    }

    struct MinimalPlugin {
        name: String,
    }

    impl Plugin for MinimalPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[voom_domain::capabilities::Capability] {
            &[]
        }
        fn handles(&self, event_type: &str) -> bool {
            event_type == voom_domain::events::Event::TOOL_DETECTED
        }
        fn on_event(
            &self,
            _event: &voom_domain::events::Event,
        ) -> voom_domain::errors::Result<Option<voom_domain::events::EventResult>> {
            Ok(Some(voom_domain::events::EventResult::new(
                self.name.clone(),
            )))
        }
    }

    #[test]
    fn set_stats_sink_swaps_the_active_sink() {
        let mut kernel = Kernel::new();
        let sink = Arc::new(CountingSink::default());

        // Register a plugin that handles TOOL_DETECTED before installing the sink.
        kernel
            .register_plugin(
                Arc::new(MinimalPlugin {
                    name: "test-plugin".into(),
                }),
                10,
            )
            .unwrap();

        // Install the new sink.
        kernel.set_stats_sink(sink.clone());

        // Dispatch one event the plugin handles.
        let event =
            voom_domain::events::Event::ToolDetected(voom_domain::events::ToolDetectedEvent::new(
                "test-tool",
                "1.0.0",
                "/usr/bin/test-tool".into(),
            ));
        kernel.dispatch(event);

        // The new sink must have seen exactly one record.
        assert_eq!(
            *sink.0.lock().unwrap(),
            1,
            "CountingSink should record exactly one invocation"
        );
    }
}
