use std::sync::Arc;

use crate::Plugin;

/// Loads native plugins (compiled Rust trait objects).
pub struct NativePluginLoader;

impl NativePluginLoader {
    pub fn new() -> Self {
        Self
    }

    /// Wrap any Plugin implementation into an Arc<dyn Plugin>.
    pub fn load<P: Plugin + 'static>(&self, plugin: P) -> Arc<dyn Plugin> {
        Arc::new(plugin)
    }
}

impl Default for NativePluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// WASM plugin loader using wasmtime's component model.
/// Only available with the `wasm` feature.
#[cfg(feature = "wasm")]
pub mod wasm {
    use super::*;
    use crate::host::HostState;
    use crate::manifest::PluginManifest;
    use std::path::Path;
    use std::sync::Mutex;
    use voom_domain::capabilities::Capability;
    use voom_domain::errors::VoomError;
    use voom_domain::events::{Event, EventResult};

    /// Loads WASM component plugins from `.wasm` files.
    ///
    /// The loader compiles WASM components and instantiates them with host
    /// function bindings. Each loaded plugin gets its own `Store` and `HostState`.
    pub struct WasmPluginLoader {
        engine: wasmtime::Engine,
    }

    impl WasmPluginLoader {
        /// Create a new WASM plugin loader with component model support enabled.
        pub fn new() -> Result<Self, VoomError> {
            let mut config = wasmtime::Config::new();
            config.wasm_component_model(true);
            let engine = wasmtime::Engine::new(&config)
                .map_err(|e| VoomError::Wasm(format!("failed to create engine: {e}")))?;
            Ok(Self { engine })
        }

        /// Load a WASM plugin from a `.wasm` file alongside a `plugin.toml` manifest.
        ///
        /// The manifest file should be at the same path as the `.wasm` file but
        /// with a `.toml` extension (e.g., `my-plugin.wasm` + `my-plugin.toml`).
        pub fn load(&self, wasm_path: &Path) -> Result<Arc<dyn Plugin>, VoomError> {
            self.load_with_host_state(wasm_path, None)
        }

        /// Load a WASM plugin with a custom HostState configuration.
        pub fn load_with_host_state(
            &self,
            wasm_path: &Path,
            host_state: Option<HostState>,
        ) -> Result<Arc<dyn Plugin>, VoomError> {
            let wasm_bytes = std::fs::read(wasm_path)
                .map_err(|e| VoomError::Wasm(format!("failed to read {}: {e}", wasm_path.display())))?;

            // Try to load the manifest from a sibling .toml file.
            let manifest = load_manifest(wasm_path)?;

            // Compile the WASM component.
            let component = wasmtime::component::Component::new(&self.engine, &wasm_bytes)
                .map_err(|e| VoomError::Wasm(format!("failed to compile component: {e}")))?;

            // Set up the linker with host function imports.
            let mut linker: wasmtime::component::Linker<HostState> =
                wasmtime::component::Linker::new(&self.engine);
            register_host_functions(&mut linker)
                .map_err(|e| VoomError::Wasm(format!("failed to register host functions: {e}")))?;

            let plugin_name = manifest
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| {
                    wasm_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                });

            // Create the store with host state.
            let state = host_state.unwrap_or_else(|| HostState::new(plugin_name.clone()));
            let store = wasmtime::Store::new(&self.engine, state);

            // Extract info from manifest or use defaults.
            let version = manifest
                .as_ref()
                .map(|m| m.version.clone())
                .unwrap_or_else(|| "0.0.0".to_string());
            let capabilities = manifest
                .as_ref()
                .map(|m| m.capabilities.clone())
                .unwrap_or_default();
            let handled_events = manifest
                .as_ref()
                .map(|m| m.handles_events.clone())
                .unwrap_or_default();

            Ok(Arc::new(WasmPlugin {
                name: plugin_name,
                version,
                capabilities,
                handled_events,
                inner: Mutex::new(WasmPluginInner {
                    store,
                    component,
                    linker,
                }),
            }))
        }

        /// Load all `.wasm` plugins from a directory.
        pub fn load_dir(&self, dir: &Path) -> Vec<Result<Arc<dyn Plugin>, VoomError>> {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "failed to read WASM plugins directory");
                    return vec![];
                }
            };

            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .map(|ext| ext == "wasm")
                        .unwrap_or(false)
                })
                .map(|entry| self.load(&entry.path()))
                .collect()
        }
    }

    /// Internal state for a loaded WASM plugin instance.
    #[allow(dead_code)]
    struct WasmPluginInner {
        store: wasmtime::Store<HostState>,
        component: wasmtime::component::Component,
        linker: wasmtime::component::Linker<HostState>,
    }

    /// A WASM plugin loaded from a `.wasm` component file.
    ///
    /// Wraps a wasmtime component instance and implements the Plugin trait,
    /// bridging between the kernel's event system and the WASM boundary.
    struct WasmPlugin {
        name: String,
        version: String,
        capabilities: Vec<Capability>,
        handled_events: Vec<String>,
        inner: Mutex<WasmPluginInner>,
    }

    // SAFETY: WasmPlugin uses Mutex for interior mutability, ensuring
    // exclusive access to the non-Sync wasmtime::Store.
    unsafe impl Send for WasmPlugin {}
    unsafe impl Sync for WasmPlugin {}

    impl Plugin for WasmPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> &str {
            &self.version
        }

        fn capabilities(&self) -> &[Capability] {
            &self.capabilities
        }

        fn handles(&self, event_type: &str) -> bool {
            self.handled_events.iter().any(|e| e == event_type)
        }

        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            let (event_type, payload) = voom_wit::event_to_wasm(event)
                .map_err(|e| voom_domain::errors::VoomError::Wasm(e.to_string()))?;

            let inner = self.inner.lock().unwrap();

            // Try to find and call the on-event export.
            // The component may not be instantiated yet (lazy instantiation).
            match call_on_event(&inner, &event_type, &payload) {
                Ok(Some((plugin_name, produced, data))) => {
                    let result = voom_wit::event_result_from_wasm(plugin_name, produced, data)
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

    /// Call the on-event export of a WASM component.
    ///
    /// This is a placeholder that returns None until full component instantiation
    /// is wired up. The actual implementation will use the component's typed
    /// function exports via wasmtime::component::TypedFunc.
    fn call_on_event(
        _inner: &WasmPluginInner,
        _event_type: &str,
        _payload: &[u8],
    ) -> Result<Option<(String, Vec<(String, Vec<u8>)>, Option<Vec<u8>>)>, anyhow::Error> {
        // TODO: Instantiate the component (lazily) and call its on-event export.
        // This requires the WASM component to implement the voom:plugin/plugin interface.
        //
        // The flow:
        // 1. linker.instantiate(&mut store, &component) -> Instance
        // 2. instance.get_typed_func::<(EventData,), (Option<EventResult>,)>("on-event")
        // 3. func.call(&mut store, (event_data,))
        // 4. Convert result back to domain types
        //
        // For now, return None (no-op) since we can't instantiate without a real
        // WASM component binary to test against.
        Ok(None)
    }

    /// Register host function imports in the linker.
    ///
    /// These are the functions that WASM plugins can call back into the host.
    fn register_host_functions(
        linker: &mut wasmtime::component::Linker<HostState>,
    ) -> Result<(), anyhow::Error> {
        // Register the host interface functions.
        // The interface name in WIT is "host" in package "voom:plugin".
        let mut root = linker.root();
        let mut host_instance = root.instance("voom:plugin/host@0.1.0")?;

        // log: func(level: log-level, message: string)
        host_instance.func_wrap(
            "log",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (level, message): (u32, String)| {
                let level_str = match level {
                    0 => "trace",
                    1 => "debug",
                    2 => "info",
                    3 => "warn",
                    4 => "error",
                    _ => "info",
                };
                ctx.data().log(level_str, &message);
                Ok(())
            },
        )?;

        // get-plugin-data: func(key: string) -> option<list<u8>>
        host_instance.func_wrap(
            "get-plugin-data",
            |ctx: wasmtime::StoreContextMut<'_, HostState>, (key,): (String,)| {
                let result = ctx.data().get_plugin_data(&key);
                Ok((result,))
            },
        )?;

        // set-plugin-data: func(key: string, value: list<u8>) -> result<_, string>
        host_instance.func_wrap(
            "set-plugin-data",
            |mut ctx: wasmtime::StoreContextMut<'_, HostState>,
             (key, value): (String, Vec<u8>)| {
                let result = ctx.data_mut().set_plugin_data(&key, &value);
                Ok((result.map_err(|e| e.to_string()),))
            },
        )?;

        // run-tool: func(tool: string, args: list<string>, timeout-ms: u64) -> result<tool-output, string>
        host_instance.func_wrap(
            "run-tool",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (tool, args, timeout_ms): (String, Vec<String>, u64)| {
                let result = ctx.data().run_tool(&tool, &args, timeout_ms);
                match result {
                    Ok(output) => Ok(((output.exit_code, output.stdout, output.stderr),)),
                    Err(e) => Err(wasmtime::Error::msg(e)),
                }
            },
        )?;

        Ok(())
    }

    /// Try to load a plugin manifest from a `.toml` file next to the `.wasm` file.
    pub(crate) fn load_manifest(wasm_path: &Path) -> Result<Option<PluginManifest>, VoomError> {
        let manifest_path = wasm_path.with_extension("toml");
        if !manifest_path.exists() {
            return Ok(None);
        }

        let contents = std::fs::read_to_string(&manifest_path)
            .map_err(|e| VoomError::Wasm(format!("failed to read manifest: {e}")))?;

        let manifest: PluginManifest = toml::from_str(&contents)
            .map_err(|e| VoomError::Wasm(format!("failed to parse manifest: {e}")))?;

        if let Err(errors) = manifest.validate() {
            return Err(VoomError::Wasm(format!(
                "invalid manifest: {}",
                errors.join(", ")
            )));
        }

        Ok(Some(manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            Ok(Some(EventResult {
                plugin_name: self.name.clone(),
                produced_events: vec![],
                data: Some(serde_json::json!({"loaded": true})),
            }))
        }
    }

    #[test]
    fn test_native_loader() {
        let loader = NativePluginLoader::new();
        let plugin = loader.load(MockPlugin {
            name: "test".into(),
        });
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
    }
}
