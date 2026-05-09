/// WIT result type for HTTP responses crossing the WASM boundary.
#[cfg(feature = "wasm")]
type WitHttpResult = Result<(u16, Vec<(String, String)>, Vec<u8>), String>;

/// Expand `~` at the start of a path string to `$HOME`.
#[cfg(feature = "wasm")]
fn expand_tilde(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(path)
}

/// WASM plugin loader using wasmtime's component model.
/// Only available with the `wasm` feature.
#[cfg(feature = "wasm")]
pub mod wasm {
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

            let component = wasmtime::component::Component::new(&self.engine, &wasm_bytes)
                .map_err(|e| WasmLoadError::ComponentCompilation {
                    path: path_str.clone(),
                    message: e.to_string(),
                })?;

            let mut linker: wasmtime::component::Linker<HostState> =
                wasmtime::component::Linker::new(&self.engine);
            register_host_functions(&mut linker)
                .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

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
            self.load_dir_with_config_skip(
                dir,
                plugin_configs,
                &std::collections::HashSet::new(),
                None,
            )
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
}

#[cfg(test)]
mod tests {
    use crate::Plugin;
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult};

    struct MockPlugin {
        name: String,
    }

    impl Plugin for MockPlugin {
        fn name(&self) -> &str {
            &self.name
        }
        fn version(&self) -> &str {
            "1.0.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, _: &str) -> bool {
            true
        }
        fn on_event(&self, _: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            {
                let mut result = EventResult::new(self.name.clone());
                result.data = Some(serde_json::json!({"loaded": true}));
                Ok(Some(result))
            }
        }
    }

    #[test]
    fn test_mock_plugin_metadata() {
        let plugin = MockPlugin {
            name: "test".into(),
        };
        assert_eq!(plugin.name(), "test");
        assert_eq!(plugin.version(), "1.0.0");
        assert!(plugin.handles("anything"));
    }

    #[cfg(feature = "wasm")]
    mod wasm_tests {
        use super::super::wasm::*;
        use std::path::PathBuf;

        #[test]
        fn test_wasm_loader_creation() {
            let loader = WasmPluginLoader::new().unwrap();
            // Loader should be created successfully with component model enabled.
            let _ = loader;
        }

        #[test]
        fn test_wasm_loader_drops_cleanly() {
            let loader = WasmPluginLoader::new().unwrap();
            assert!(!loader.shutdown.load(std::sync::atomic::Ordering::Acquire));
            drop(loader);
        }

        #[test]
        fn test_epoch_thread_increments() {
            let loader = WasmPluginLoader::new().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(loader);
        }

        #[test]
        fn test_wasm_loader_missing_file() {
            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&PathBuf::from("/nonexistent/plugin.wasm"));
            assert!(result.is_err());
        }

        #[test]
        fn test_wasm_loader_invalid_wasm() {
            let dir = tempfile::tempdir().unwrap();
            let wasm_path = dir.path().join("bad.wasm");
            std::fs::write(&wasm_path, b"not valid wasm").unwrap();

            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&wasm_path);
            assert!(result.is_err());
        }

        #[test]
        fn test_wasm_loader_empty_dir() {
            let dir = tempfile::tempdir().unwrap();
            let loader = WasmPluginLoader::new().unwrap();
            let results = loader.load_dir(dir.path());
            assert!(results.is_empty());
        }

        #[test]
        fn test_wasm_loader_nonexistent_dir() {
            let loader = WasmPluginLoader::new().unwrap();
            let results = loader.load_dir(&PathBuf::from("/nonexistent/dir"));
            assert!(results.is_empty());
        }

        #[test]
        fn test_manifest_loading() {
            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let result = load_manifest(&wasm_path);
            // Should successfully load the manifest.
            assert!(result.is_ok());
            let manifest = result.unwrap();
            assert!(manifest.is_some());
            let manifest = manifest.unwrap();
            assert_eq!(manifest.name, "test-plugin");
            assert_eq!(manifest.version, "1.0.0");
        }

        #[test]
        fn test_manifest_missing_ok() {
            let result = load_manifest(&PathBuf::from("/nonexistent/plugin.wasm"));
            assert!(result.is_ok());
            assert!(result.unwrap().is_none());
        }

        #[cfg(unix)]
        #[test]
        fn test_manifest_world_writable_rejected() {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            // Make world-writable.
            std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o666))
                .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let result = load_manifest(&wasm_path);
            assert!(result.is_err());
            let err = format!("{}", result.unwrap_err());
            assert!(
                err.contains("world-writable"),
                "expected world-writable error, got: {err}"
            );
        }

        #[test]
        fn test_wasm_event_payload_size_limit() {
            use super::super::wasm::MAX_WASM_EVENT_PAYLOAD;

            // Verify the constant is 16 MiB.
            assert_eq!(MAX_WASM_EVENT_PAYLOAD, 16 * 1024 * 1024);

            // Create a payload larger than the limit and verify the check would catch it.
            let oversized = vec![0u8; MAX_WASM_EVENT_PAYLOAD + 1];
            assert!(
                oversized.len() > MAX_WASM_EVENT_PAYLOAD,
                "payload of {} bytes should exceed max of {}",
                oversized.len(),
                MAX_WASM_EVENT_PAYLOAD
            );
        }

        #[test]
        fn test_oversized_wasm_return_value_rejected() {
            use super::super::wasm::MAX_WASM_EVENT_PAYLOAD;

            // Simulate an oversized WasmEventResult by constructing one
            // whose total size exceeds MAX_WASM_EVENT_PAYLOAD.
            let oversized_data = vec![0u8; MAX_WASM_EVENT_PAYLOAD + 1];
            let wasm_result = voom_wit::WasmEventResult {
                plugin_name: "test-plugin".into(),
                produced_events: vec![],
                data: Some(oversized_data),
                claimed: false,
                execution_error: None,
                execution_detail: None,
            };

            let output_size: usize = wasm_result
                .produced_events
                .iter()
                .map(|(t, p)| t.len() + p.len())
                .sum::<usize>()
                + wasm_result.data.as_ref().map_or(0, Vec::len)
                + wasm_result.execution_error.as_ref().map_or(0, String::len)
                + wasm_result.execution_detail.as_ref().map_or(0, Vec::len);

            assert!(
                output_size > MAX_WASM_EVENT_PAYLOAD,
                "constructed result should exceed limit"
            );
        }

        #[test]
        fn test_protocol_version_compatible_loads() {
            use crate::manifest::CURRENT_PROTOCOL_VERSION;

            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                format!(
                    r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]
protocol_version = {CURRENT_PROTOCOL_VERSION}

[[capabilities]]
Evaluate = {{}}
"#
                ),
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let manifest = load_manifest(&wasm_path).unwrap().unwrap();
            assert_eq!(manifest.protocol_version, Some(CURRENT_PROTOCOL_VERSION));
        }

        #[test]
        fn test_protocol_version_none_is_compatible() {
            let dir = tempfile::tempdir().unwrap();
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]

[[capabilities]]
Evaluate = {}
"#,
            )
            .unwrap();

            let wasm_path = dir.path().join("test-plugin.wasm");
            let manifest = load_manifest(&wasm_path).unwrap().unwrap();
            assert!(
                manifest.protocol_version.is_none(),
                "missing protocol_version should default to None"
            );
        }

        /// Verify that a manifest with explicit `allowed_paths = []` clears
        /// any config-provided paths, enforcing deny-all filesystem access.
        #[test]
        fn test_manifest_explicit_empty_allowed_paths_clears_config_paths() {
            use crate::host::HostState;
            use crate::manifest::PluginManifest;

            let mut state =
                HostState::new("test-plugin".into()).with_paths(vec!["/some/config/path".into()]);
            assert!(!state.allowed_paths.is_empty());

            let manifest: PluginManifest = toml::from_str(
                r#"
name = "test-plugin"
version = "1.0.0"
description = "test"
capabilities = []
handles_events = []
allowed_paths = []
"#,
            )
            .expect("valid manifest TOML");
            assert!(
                manifest.allowed_paths.is_some(),
                "explicit empty array should deserialize to Some([])"
            );

            // Apply the same logic as load_with_manifest.
            if let Some(ref paths) = manifest.allowed_paths {
                state.allowed_paths = paths
                    .iter()
                    .map(|p| super::super::expand_tilde(p))
                    .collect();
            }

            assert!(
                state.allowed_paths.is_empty(),
                "manifest with allowed_paths = [] must clear config-provided paths"
            );
        }

        /// Verify that a manifest that omits `allowed_paths` preserves
        /// host-configured filesystem access paths.
        #[test]
        fn test_manifest_omitted_allowed_paths_keeps_config_paths() {
            use crate::host::HostState;
            use crate::manifest::PluginManifest;

            let mut state =
                HostState::new("test-plugin".into()).with_paths(vec!["/some/config/path".into()]);

            let manifest: PluginManifest = toml::from_str(
                r#"
name = "test-plugin"
version = "1.0.0"
description = "test"
capabilities = []
handles_events = []
"#,
            )
            .expect("valid manifest TOML");
            assert!(
                manifest.allowed_paths.is_none(),
                "omitted field should deserialize to None"
            );

            if let Some(ref paths) = manifest.allowed_paths {
                state.allowed_paths = paths
                    .iter()
                    .map(|p| super::super::expand_tilde(p))
                    .collect();
            }

            assert_eq!(
                state.allowed_paths.len(),
                1,
                "omitted allowed_paths must preserve config-provided paths"
            );
        }

        #[test]
        fn test_incompatible_protocol_version_fails_load() {
            use crate::manifest::CURRENT_PROTOCOL_VERSION;

            let dir = tempfile::tempdir().unwrap();
            let bad_version = CURRENT_PROTOCOL_VERSION + 99;
            let manifest_path = dir.path().join("test-plugin.toml");
            std::fs::write(
                &manifest_path,
                format!(
                    r#"
name = "test-plugin"
version = "1.0.0"
description = "A test WASM plugin"
handles_events = ["file.discovered"]
protocol_version = {bad_version}

[[capabilities]]
Evaluate = {{}}
"#
                ),
            )
            .unwrap();

            // Write a minimal (invalid) WASM file so load_with_manifest gets past file read
            let wasm_path = dir.path().join("test-plugin.wasm");
            std::fs::write(&wasm_path, b"not valid wasm").unwrap();

            let loader = WasmPluginLoader::new().unwrap();
            let result = loader.load(&wasm_path);
            let err = match result {
                Err(e) => format!("{e}"),
                Ok(_) => panic!("expected error for incompatible protocol version"),
            };
            assert!(
                err.contains("incompatible protocol_version"),
                "expected protocol version error, got: {err}"
            );
        }
    }
}
