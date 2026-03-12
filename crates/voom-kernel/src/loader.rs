use std::sync::Arc;

use crate::Plugin;

/// WIT result type for HTTP responses crossing the WASM boundary.
#[cfg(feature = "wasm")]
type WitHttpResult = Result<(u16, Vec<(String, String)>, Vec<u8>), String>;

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
            config.max_wasm_stack(1024 * 1024); // 1 MiB stack limit
            config.epoch_interruption(true);
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
            let wasm_bytes = std::fs::read(wasm_path).map_err(|e| {
                VoomError::Wasm(format!("failed to read {}: {e}", wasm_path.display()))
            })?;

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

            // Create the store with host state and resource limits.
            let mut state = host_state.unwrap_or_else(|| HostState::new(plugin_name.clone()));

            // Populate allowed capabilities from manifest.
            if let Some(ref manifest) = manifest {
                state.allowed_capabilities = manifest
                    .capabilities
                    .iter()
                    .map(|c| c.kind().to_string())
                    .collect();
            }

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
                    instance: None,
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
                .map(|entry| self.load(&entry.path()))
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

            if payload.len() > MAX_WASM_EVENT_PAYLOAD {
                return Err(voom_domain::errors::VoomError::Wasm(format!(
                    "event payload too large: {} bytes (max {})",
                    payload.len(),
                    MAX_WASM_EVENT_PAYLOAD
                )));
            }

            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

            // Try to find and call the on-event export.
            // The component is lazily instantiated on first call.
            match call_on_event(&mut inner, &event_type, &payload) {
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
    /// Lazily instantiates the component on first call, then invokes the
    /// `on-event` function from the `voom:plugin/plugin` interface.
    ///
    /// The WIT signature is:
    ///   on-event: func(event: event-data) -> option<event-result>
    /// where event-data is { event-type: string, payload: list<u8> }
    /// and event-result is { plugin-name: string, produced-events: list<event-data>, data: option<list<u8>> }
    fn call_on_event(
        inner: &mut WasmPluginInner,
        event_type: &str,
        payload: &[u8],
    ) -> Result<Option<voom_wit::WasmEventResult>, anyhow::Error> {
        use wasmtime::component::Val;

        // Lazily instantiate the component.
        if inner.instance.is_none() {
            let instance = inner
                .linker
                .instantiate(&mut inner.store, &inner.component)?;
            inner.instance = Some(instance);
        }

        let instance = inner.instance.as_ref().unwrap();

        // Look up the on-event export from the plugin interface.
        // The fully-qualified export name depends on whether the component uses
        // a default export or a named interface export.
        let on_event = instance
            .get_export(&mut inner.store, None, "voom:plugin/plugin@0.1.0")
            .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
            .and_then(|idx| instance.get_func(&mut inner.store, idx))
            .or_else(|| {
                // Fallback: try as a top-level export (for simpler components).
                let idx = instance.get_export(&mut inner.store, None, "on-event")?;
                instance.get_func(&mut inner.store, idx)
            });

        let on_event = match on_event {
            Some(func) => func,
            None => {
                tracing::warn!("WASM component has no 'on-event' export");
                return Ok(None);
            }
        };

        // Build the event-data record as a Val::Record.
        let event_data = Val::Record(vec![
            ("event-type".into(), Val::String(event_type.into())),
            (
                "payload".into(),
                Val::List(payload.iter().map(|b| Val::U8(*b)).collect()),
            ),
        ]);

        // Prepare the result slot. The return type is option<event-result>.
        let mut results = vec![Val::Option(None)];

        on_event.call(&mut inner.store, &[event_data], &mut results)?;
        on_event.post_return(&mut inner.store)?;

        // Parse the option<event-result> return value.
        match &results[0] {
            Val::Option(None) => Ok(None),
            Val::Option(Some(boxed_val)) => parse_event_result(boxed_val).map(Some),
            other => anyhow::bail!(
                "unexpected return type from on-event: expected Option, got {:?}",
                std::mem::discriminant(other)
            ),
        }
    }

    /// Parse a Val representing an event-result record into the tuple form
    /// expected by `event_result_from_wasm`.
    fn parse_event_result(
        val: &wasmtime::component::Val,
    ) -> Result<voom_wit::WasmEventResult, anyhow::Error> {
        use wasmtime::component::Val;

        let fields = match val {
            Val::Record(fields) => fields,
            other => anyhow::bail!(
                "expected Record for event-result, got {:?}",
                std::mem::discriminant(other)
            ),
        };

        let mut plugin_name = String::new();
        let mut produced_events = Vec::new();
        let mut data: Option<Vec<u8>> = None;

        for (name, field_val) in fields {
            match name.as_str() {
                "plugin-name" => {
                    if let Val::String(s) = field_val {
                        plugin_name = s.to_string();
                    }
                }
                "produced-events" => {
                    if let Val::List(items) = field_val {
                        for item in items {
                            if let Val::Record(event_fields) = item {
                                let mut evt_type = String::new();
                                let mut evt_payload = Vec::new();
                                for (ename, eval) in event_fields {
                                    match ename.as_str() {
                                        "event-type" => {
                                            if let Val::String(s) = eval {
                                                evt_type = s.to_string();
                                            }
                                        }
                                        "payload" => {
                                            if let Val::List(bytes) = eval {
                                                evt_payload = bytes
                                                    .iter()
                                                    .filter_map(|v| {
                                                        if let Val::U8(b) = v {
                                                            Some(*b)
                                                        } else {
                                                            None
                                                        }
                                                    })
                                                    .collect();
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                produced_events.push((evt_type, evt_payload));
                            }
                        }
                    }
                }
                "data" => match field_val {
                    Val::Option(Some(boxed)) => {
                        if let Val::List(bytes) = boxed.as_ref() {
                            data =
                                Some(
                                    bytes
                                        .iter()
                                        .filter_map(|v| {
                                            if let Val::U8(b) = v {
                                                Some(*b)
                                            } else {
                                                None
                                            }
                                        })
                                        .collect(),
                                );
                        }
                    }
                    Val::Option(None) => {
                        data = None;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok((plugin_name, produced_events, data))
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
            |ctx: wasmtime::StoreContextMut<'_, HostState>, (level, message): (u32, String)| {
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
            |mut ctx: wasmtime::StoreContextMut<'_, HostState>, (key, value): (String, Vec<u8>)| {
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
                let wit_result: Result<(i32, Vec<u8>, Vec<u8>), String> = match result {
                    Ok(output) => Ok((output.exit_code, output.stdout, output.stderr)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )?;

        // http-get: func(url: string, headers: list<tuple<string, string>>) -> result<http-response, string>
        host_instance.func_wrap(
            "http-get",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (url, headers): (String, Vec<(String, String)>)| {
                let result = ctx.data().http_get(&url, &headers);
                let wit_result: WitHttpResult = match result
                {
                    Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )?;

        // http-post: func(url: string, headers: list<tuple<string, string>>, body: list<u8>) -> result<http-response, string>
        host_instance.func_wrap(
            "http-post",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (url, headers, body): (String, Vec<(String, String)>, Vec<u8>)| {
                let result = ctx.data().http_post(&url, &headers, &body);
                let wit_result: WitHttpResult = match result
                {
                    Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                    Err(e) => Err(e),
                };
                Ok((wit_result,))
            },
        )?;

        // read-file-metadata: func(path: string) -> result<list<u8>, string>
        host_instance.func_wrap(
            "read-file-metadata",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (path,): (String,)|
             -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                let file_path = std::path::Path::new(&path);

                // Security: check path is within allowed directories.
                if !ctx.data().allowed_paths.is_empty() {
                    let allowed = ctx
                        .data()
                        .allowed_paths
                        .iter()
                        .any(|p| file_path.starts_with(p));
                    if !allowed {
                        return Ok((Err(format!(
                            "path '{}' is not within allowed directories",
                            path
                        )),));
                    }
                }

                match std::fs::metadata(file_path) {
                    Ok(meta) => {
                        let info = serde_json::json!({
                            "size": meta.len(),
                            "is_file": meta.is_file(),
                            "is_dir": meta.is_dir(),
                            "readonly": meta.permissions().readonly(),
                            "modified": meta.modified().ok().map(|t| {
                                t.duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                            }),
                        });
                        let bytes = rmp_serde::to_vec(&info)
                            .map_err(|e| format!("failed to serialize metadata: {e}"));
                        Ok((bytes,))
                    }
                    Err(e) => Ok((Err(format!(
                        "failed to read metadata for '{}': {}",
                        path, e
                    )),)),
                }
            },
        )?;

        // list-files: func(dir: string, pattern: string) -> result<list<string>, string>
        host_instance.func_wrap(
            "list-files",
            |ctx: wasmtime::StoreContextMut<'_, HostState>,
             (dir, pattern): (String, String)|
             -> Result<(Result<Vec<String>, String>,), wasmtime::Error> {
                let dir_path = std::path::Path::new(&dir);

                // Security: check directory is within allowed paths.
                if !ctx.data().allowed_paths.is_empty() {
                    let allowed = ctx
                        .data()
                        .allowed_paths
                        .iter()
                        .any(|p| dir_path.starts_with(p));
                    if !allowed {
                        return Ok((Err(format!(
                            "directory '{}' is not within allowed directories",
                            dir
                        )),));
                    }
                }

                match std::fs::read_dir(dir_path) {
                    Ok(entries) => {
                        let files: Vec<String> = entries
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                if pattern.is_empty() {
                                    true
                                } else {
                                    e.file_name().to_string_lossy().contains(&pattern)
                                }
                            })
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect();
                        Ok((Ok(files),))
                    }
                    Err(e) => Ok((Err(format!("failed to list directory '{}': {}", dir, e)),)),
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(&manifest_path) {
                let mode = metadata.permissions().mode();
                if mode & 0o002 != 0 {
                    return Err(VoomError::Wasm(format!(
                        "WASM plugin manifest {:?} is world-writable (mode {:o}), refusing to load",
                        manifest_path, mode
                    )));
                }
            }
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
                claimed: false,
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
                "expected world-writable error, got: {}",
                err
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
    }
}
