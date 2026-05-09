use std::sync::Arc;

use crate::errors::WasmLoadError;
use crate::host::HostState;
use crate::manifest::PluginManifest;
use crate::Plugin;
mod host_imports;
mod host_state_config;
mod wit_runtime;

use host_imports::register_host_functions;
use host_state_config::{
    attach_storage, configure_manifest_permissions, host_state_from_config, manifest_metadata,
    plugin_name_from_manifest,
};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use voom_domain::errors::VoomError;
use voom_domain::events::{Event, EventResult};
use wit_runtime::call_on_event;

/// A loaded plugin paired with its manifest-declared priority.
pub type PluginWithPriority = (Arc<dyn Plugin>, i32);

/// Interval between epoch increments (10ms).
/// With the default deadline of 200 ticks, this gives a 2-second timeout
/// for WASM execution (200 ticks * 10ms = 2000ms).
const EPOCH_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// Loads WASM component plugins from `.wasm` files.
///
/// The loader compiles WASM components and instantiates them with host
/// function bindings. Each loaded plugin gets its own `Store` and `HostState`.
///
/// A background thread increments the engine epoch every 10ms. With the
/// default deadline of 200 ticks, WASM execution is interrupted after ~2s.
pub struct WasmPluginLoader {
    engine: Arc<wasmtime::Engine>,
    pub(crate) shutdown: Arc<AtomicBool>,
    epoch_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WasmPluginLoader {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.epoch_thread.take() {
            handle.join().ok();
        }
    }
}

impl WasmPluginLoader {
    /// Create a new WASM plugin loader with component model support enabled.
    ///
    /// Spawns a background thread that calls `engine.increment_epoch()`
    /// every 10ms to enforce execution timeouts on WASM plugins.
    pub fn new() -> Result<Self, WasmLoadError> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        config.max_wasm_stack(1024 * 1024); // 1 MiB stack limit
        config.epoch_interruption(true);
        let engine = Arc::new(
            wasmtime::Engine::new(&config)
                .map_err(|e| WasmLoadError::EngineCreation(e.to_string()))?,
        );

        let shutdown = Arc::new(AtomicBool::new(false));
        let epoch_engine = Arc::clone(&engine);
        let epoch_shutdown = Arc::clone(&shutdown);
        let epoch_thread = std::thread::Builder::new()
            .name("wasm-epoch-ticker".into())
            .spawn(move || {
                while !epoch_shutdown.load(Ordering::Acquire) {
                    std::thread::sleep(EPOCH_TICK_INTERVAL);
                    epoch_engine.increment_epoch();
                }
            })
            .map_err(|e| {
                WasmLoadError::EngineCreation(format!("failed to spawn epoch thread: {e}"))
            })?;

        Ok(Self {
            engine,
            shutdown,
            epoch_thread: Some(epoch_thread),
        })
    }

    /// Load a WASM plugin from a `.wasm` file alongside a `plugin.toml` manifest.
    ///
    /// The manifest file should be at the same path as the `.wasm` file but
    /// with a `.toml` extension (e.g., `my-plugin.wasm` + `my-plugin.toml`).
    pub fn load(&self, wasm_path: &Path) -> Result<Arc<dyn Plugin>, WasmLoadError> {
        self.load_with_host_state(wasm_path, None)
    }

    /// Load a WASM plugin with a custom `HostState` configuration.
    pub fn load_with_host_state(
        &self,
        wasm_path: &Path,
        host_state: Option<HostState>,
    ) -> Result<Arc<dyn Plugin>, WasmLoadError> {
        let manifest = load_manifest(wasm_path)?;
        self.load_with_manifest(wasm_path, manifest, host_state)
    }

    /// Load a WASM plugin with a pre-loaded manifest and custom `HostState`.
    ///
    /// This avoids re-reading the manifest when the caller has already loaded
    /// it (e.g. `load_dir_with_config` reads the manifest to determine the
    /// plugin name for config lookup).
    ///
    /// **Note:** `allowed_capabilities` from the manifest always replaces any
    /// value set on the incoming `host_state`. This ensures the plugin cannot
    /// escalate its own capabilities via config.
    pub fn load_with_manifest(
        &self,
        wasm_path: &Path,
        manifest: Option<PluginManifest>,
        host_state: Option<HostState>,
    ) -> Result<Arc<dyn Plugin>, WasmLoadError> {
        let path_str = wasm_path.display().to_string();

        if let Some(ref m) = manifest {
            if let Some(v) = m.protocol_version {
                if v != crate::manifest::CURRENT_PROTOCOL_VERSION {
                    return Err(WasmLoadError::ManifestInvalid {
                        path: path_str,
                        message: format!(
                            "incompatible protocol_version {v} \
                             (expected {}, or omit for backward compat)",
                            crate::manifest::CURRENT_PROTOCOL_VERSION
                        ),
                    });
                }
            }
        }

        let wasm_bytes = std::fs::read(wasm_path).map_err(|e| WasmLoadError::ReadFile {
            path: path_str.clone(),
            source: e,
        })?;

        const MAX_WASM_SIZE: usize = 256 * 1024 * 1024; // 256 MiB
        if wasm_bytes.len() > MAX_WASM_SIZE {
            return Err(WasmLoadError::FileTooLarge {
                path: path_str.clone(),
                size: wasm_bytes.len(),
                max: MAX_WASM_SIZE,
            });
        }

        let component =
            wasmtime::component::Component::new(&self.engine, &wasm_bytes).map_err(|e| {
                WasmLoadError::ComponentCompilation {
                    path: path_str.clone(),
                    message: e.to_string(),
                }
            })?;

        let mut linker: wasmtime::component::Linker<HostState> =
            wasmtime::component::Linker::new(&self.engine);
        register_host_functions(&mut linker).map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        let plugin_name = plugin_name_from_manifest(manifest.as_ref(), wasm_path);

        let mut state = configure_manifest_permissions(
            host_state.unwrap_or_else(|| HostState::new(plugin_name.clone())),
            manifest.as_ref(),
        );

        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(state.wasm_limits.max_memory_bytes)
            .table_elements(10_000)
            .instances(10)
            .memories(1)
            .build();
        state.store_limits = limits;

        let mut store = wasmtime::Store::new(&self.engine, state);
        store.limiter(|s| &mut s.store_limits);
        store.set_epoch_deadline(store.data().wasm_limits.epoch_deadline_ticks);

        let metadata = manifest_metadata(manifest.as_ref());

        Ok(Arc::new(WasmPlugin {
            name: plugin_name,
            version: metadata.version,
            description: metadata.description,
            author: metadata.author,
            license: metadata.license,
            homepage: metadata.homepage,
            capabilities: metadata.capabilities,
            handled_events: metadata.handled_events,
            inner: Mutex::new(WasmPluginInner {
                store,
                component,
                linker,
                instance: None,
            }),
        }))
    }

    /// Load all `.wasm` plugins from a directory.
    pub fn load_dir(&self, dir: &Path) -> Vec<Result<Arc<dyn Plugin>, WasmLoadError>> {
        self.load_dir_with_config(dir, &std::collections::HashMap::new())
    }

    /// Load all `.wasm` plugins from a directory, seeding each plugin's
    /// `HostState` with its per-plugin configuration from the provided map.
    ///
    /// The `plugin_configs` keys should match plugin names (from their
    /// manifest `.toml` file or the `.wasm` filename stem).
    pub fn load_dir_with_config(
        &self,
        dir: &Path,
        plugin_configs: &std::collections::HashMap<String, toml::Table>,
    ) -> Vec<Result<Arc<dyn Plugin>, WasmLoadError>> {
        self.load_dir_with_config_skip(dir, plugin_configs, &std::collections::HashSet::new(), None)
            .into_iter()
            .map(|r| r.map(|(plugin, _priority)| plugin))
            .collect()
    }

    /// Like [`WasmPluginLoader::load_dir_with_config`] but skips plugins in the `skip_plugins` set
    /// before compilation, and returns `(plugin, priority)` tuples where
    /// priority comes from the manifest (default: 70).
    ///
    /// When `storage` is provided, wires storage-backed plugin data and
    /// transition history into each plugin's [`HostState`].
    pub fn load_dir_with_config_skip(
        &self,
        dir: &Path,
        plugin_configs: &std::collections::HashMap<String, toml::Table>,
        skip_plugins: &std::collections::HashSet<String>,
        storage: Option<Arc<dyn voom_domain::storage::StorageTrait>>,
    ) -> Vec<Result<PluginWithPriority, WasmLoadError>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "failed to read WASM plugins directory");
                return vec![];
            }
        };

        entries
            .filter_map(|entry| {
                entry.map_err(|e| {
                    tracing::warn!(error = %e, "failed to read directory entry in WASM plugins dir");
                }).ok()
            })
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .map(|ext| ext == "wasm")
                    .unwrap_or(false)
            })
            .filter_map(|entry| {
                let wasm_path = entry.path();
                let manifest = match load_manifest(&wasm_path) {
                    Ok(m) => m,
                    Err(e) => return Some(Err(e)),
                };

                let plugin_name =
                    plugin_name_from_manifest(manifest.as_ref(), &wasm_path);

                if skip_plugins.contains(&plugin_name) {
                    tracing::info!(plugin = %plugin_name, "WASM plugin disabled, skipping load");
                    return None;
                }

                let mut host_state = plugin_configs
                    .get(&plugin_name)
                    .map(|table| host_state_from_config(&plugin_name, table));
                attach_storage(&mut host_state, &plugin_name, storage.as_ref());

                let priority = manifest.as_ref().map(|m| m.priority).unwrap_or(70);
                Some(
                    self.load_with_manifest(&wasm_path, manifest, host_state)
                        .map(|plugin| (plugin, priority)),
                )
            })
            .collect()
    }
}

/// Internal state for a loaded WASM plugin instance.
struct WasmPluginInner {
    store: wasmtime::Store<HostState>,
    component: wasmtime::component::Component,
    linker: wasmtime::component::Linker<HostState>,
    /// Lazily instantiated component instance.
    instance: Option<wasmtime::component::Instance>,
}

/// Maximum size for WASM event payloads (16 MiB).
pub(crate) const MAX_WASM_EVENT_PAYLOAD: usize = 16 * 1024 * 1024;

/// A WASM plugin loaded from a `.wasm` component file.
///
/// Wraps a wasmtime component instance and implements the Plugin trait,
/// bridging between the kernel's event system and the WASM boundary.
struct WasmPlugin {
    name: String,
    version: String,
    description: String,
    author: String,
    license: String,
    homepage: String,
    capabilities: Vec<voom_domain::capabilities::Capability>,
    handled_events: Vec<String>,
    inner: Mutex<WasmPluginInner>,
}

// SAFETY: WasmPlugin uses Mutex for interior mutability, ensuring
// exclusive access to the non-Send wasmtime::Store from any thread.
unsafe impl Send for WasmPlugin {}
// SAFETY: All access to the non-Sync wasmtime::Store goes through the
// internal Mutex, so concurrent &WasmPlugin references cannot race.
unsafe impl Sync for WasmPlugin {}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn author(&self) -> &str {
        &self.author
    }

    fn license(&self) -> &str {
        &self.license
    }

    fn homepage(&self) -> &str {
        &self.homepage
    }

    fn capabilities(&self) -> &[voom_domain::capabilities::Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        self.handled_events.iter().any(|e| e == event_type)
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        let (event_type, payload) = voom_wit::event_to_wasm(event)
            .map_err(|e| voom_domain::errors::VoomError::Wasm(e.to_string()))?;

        if payload.len() > MAX_WASM_EVENT_PAYLOAD {
            return Err(voom_domain::errors::VoomError::Wasm(format!(
                "event payload too large: {} bytes (max {})",
                payload.len(),
                MAX_WASM_EVENT_PAYLOAD
            )));
        }

        let mut inner = self.inner.lock();

        match call_on_event(&mut inner, &event_type, &payload) {
            Ok(Some(wasm_result)) => {
                let output_size: usize = wasm_result
                    .produced_events
                    .iter()
                    .map(|(t, p)| t.len() + p.len())
                    .sum::<usize>()
                    + wasm_result.data.as_ref().map_or(0, Vec::len)
                    + wasm_result.execution_error.as_ref().map_or(0, String::len)
                    + wasm_result.execution_detail.as_ref().map_or(0, Vec::len);
                if output_size > MAX_WASM_EVENT_PAYLOAD {
                    return Err(voom_domain::errors::VoomError::Wasm(format!(
                        "WASM plugin '{}' returned oversized payload: \
                             {} bytes (max {})",
                        self.name, output_size, MAX_WASM_EVENT_PAYLOAD
                    )));
                }
                let result = voom_wit::event_result_from_wasm(
                    wasm_result.plugin_name,
                    wasm_result.produced_events,
                    wasm_result.data,
                    wasm_result.claimed,
                    wasm_result.execution_error,
                    wasm_result.execution_detail,
                )
                .map_err(|e| voom_domain::errors::VoomError::Wasm(e.to_string()))?;
                Ok(Some(result))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                tracing::error!(
                    plugin = %self.name,
                    error = %e,
                    "WASM plugin on_event failed"
                );
                Err(voom_domain::errors::VoomError::Wasm(format!(
                    "WASM plugin '{}' on_event failed: {}",
                    self.name, e
                )))
            }
        }
    }
}

/// Try to load a plugin manifest from a `.toml` file next to the `.wasm` file.
pub(crate) fn load_manifest(wasm_path: &Path) -> Result<Option<PluginManifest>, WasmLoadError> {
    let manifest_path = wasm_path.with_extension("toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let manifest_str = manifest_path.display().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&manifest_path) {
            let mode = metadata.permissions().mode();
            if mode & 0o002 != 0 {
                return Err(WasmLoadError::ManifestWorldWritable {
                    path: manifest_str.clone(),
                    mode,
                });
            }
        }
    }

    let contents =
        std::fs::read_to_string(&manifest_path).map_err(|e| WasmLoadError::ManifestRead {
            path: manifest_str.clone(),
            source: e,
        })?;

    let manifest: PluginManifest =
        toml::from_str(&contents).map_err(|e| WasmLoadError::ManifestParse {
            path: manifest_str.clone(),
            message: e.to_string(),
        })?;

    if let Err(errors) = manifest.validate() {
        return Err(WasmLoadError::ManifestInvalid {
            path: manifest_str,
            message: errors.join(", "),
        });
    }

    Ok(Some(manifest))
}

/// Convert a `WasmLoadError` into a `VoomError` for use in event handlers
/// where the `Plugin::on_event` return type requires `VoomError`.
impl From<WasmLoadError> for VoomError {
    fn from(e: WasmLoadError) -> Self {
        VoomError::Wasm(e.to_string())
    }
}
