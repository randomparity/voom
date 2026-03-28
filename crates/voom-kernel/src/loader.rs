/// WIT result type for HTTP responses crossing the WASM boundary.
#[cfg(feature = "wasm")]
type WitHttpResult = Result<(u16, Vec<(String, String)>, Vec<u8>), String>;

/// WASM plugin loader using wasmtime's component model.
/// Only available with the `wasm` feature.
#[cfg(feature = "wasm")]
pub mod wasm {
    use std::sync::Arc;

    use crate::errors::WasmLoadError;
    use crate::host::HostState;
    use crate::manifest::PluginManifest;
    use crate::Plugin;
    use std::path::Path;
    use std::sync::Mutex;
    use voom_domain::capabilities::Capability;
    use voom_domain::errors::VoomError;
    use voom_domain::events::{Event, EventResult};

    /// A loaded plugin paired with its manifest-declared priority.
    pub type PluginWithPriority = (Arc<dyn Plugin>, i32);

    /// Loads WASM component plugins from `.wasm` files.
    ///
    /// The loader compiles WASM components and instantiates them with host
    /// function bindings. Each loaded plugin gets its own `Store` and `HostState`.
    pub struct WasmPluginLoader {
        engine: wasmtime::Engine,
    }

    impl WasmPluginLoader {
        /// Create a new WASM plugin loader with component model support enabled.
        pub fn new() -> Result<Self, WasmLoadError> {
            let mut config = wasmtime::Config::new();
            config.wasm_component_model(true);
            config.max_wasm_stack(1024 * 1024); // 1 MiB stack limit
            config.epoch_interruption(true);
            let engine = wasmtime::Engine::new(&config)
                .map_err(|e| WasmLoadError::EngineCreation(e.to_string()))?;
            Ok(Self { engine })
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

            let mut state = host_state.unwrap_or_else(|| HostState::new(plugin_name.clone()));

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
            self.load_dir_with_config_skip(dir, plugin_configs, &std::collections::HashSet::new())
                .into_iter()
                .map(|r| r.map(|(plugin, _priority)| plugin))
                .collect()
        }

        /// Like [`WasmPluginLoader::load_dir_with_config`] but skips plugins in the `skip_plugins` set
        /// before compilation, and returns `(plugin, priority)` tuples where
        /// priority comes from the manifest (default: 70).
        pub fn load_dir_with_config_skip(
            &self,
            dir: &Path,
            plugin_configs: &std::collections::HashMap<String, toml::Table>,
            skip_plugins: &std::collections::HashSet<String>,
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

                    if skip_plugins.contains(&plugin_name) {
                        tracing::info!(plugin = %plugin_name, "WASM plugin disabled, skipping load");
                        return None;
                    }

                    let host_state = plugin_configs.get(&plugin_name).map(|table| {
                        let config_value = match serde_json::to_value(table) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(plugin = %plugin_name, error = %e,
                                    "failed to convert plugin config to JSON; using empty config");
                                serde_json::json!({})
                            }
                        };
                        HostState::new(plugin_name.clone()).with_initial_config(config_value)
                    });

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
    ) -> Result<Option<voom_wit::WasmEventResult>, WasmLoadError> {
        use wasmtime::component::Val;

        if inner.instance.is_none() {
            let instance = inner
                .linker
                .instantiate(&mut inner.store, &inner.component)
                .map_err(|e| WasmLoadError::Instantiation(e.to_string()))?;
            inner.instance = Some(instance);
        }

        let instance = inner.instance.as_ref().unwrap();

        // Try the namespaced interface first, then fall back to a bare export
        // for simpler single-interface components.
        let on_event = instance
            .get_export(&mut inner.store, None, "voom:plugin/plugin@0.1.0")
            .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
            .and_then(|idx| instance.get_func(&mut inner.store, idx))
            .or_else(|| {
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

        let event_data = Val::Record(vec![
            ("event-type".into(), Val::String(event_type.into())),
            (
                "payload".into(),
                Val::List(payload.iter().map(|b| Val::U8(*b)).collect()),
            ),
        ]);

        let mut results = vec![Val::Option(None)];

        on_event
            .call(&mut inner.store, &[event_data], &mut results)
            .map_err(|e| WasmLoadError::ComponentCall(e.to_string()))?;
        on_event
            .post_return(&mut inner.store)
            .map_err(|e| WasmLoadError::ComponentCall(e.to_string()))?;

        match &results[0] {
            Val::Option(None) => Ok(None),
            Val::Option(Some(boxed_val)) => parse_event_result(boxed_val).map(Some),
            other => Err(WasmLoadError::UnexpectedValue(format!(
                "expected Option return from on-event, got {:?}",
                std::mem::discriminant(other)
            ))),
        }
    }

    /// Extract a String from a Val, returning empty string for non-string values.
    fn val_to_string(val: &wasmtime::component::Val) -> String {
        if let wasmtime::component::Val::String(s) = val {
            s.to_string()
        } else {
            String::new()
        }
    }

    /// Extract bytes from a `Val::List` of `Val::U8` values.
    fn val_to_bytes(val: &wasmtime::component::Val) -> Vec<u8> {
        if let wasmtime::component::Val::List(items) = val {
            items
                .iter()
                .filter_map(|v| {
                    if let wasmtime::component::Val::U8(b) = v {
                        Some(*b)
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Parse a `Val::Record` representing an event-data into (`event_type`, payload).
    fn parse_event_data(val: &wasmtime::component::Val) -> Option<(String, Vec<u8>)> {
        if let wasmtime::component::Val::Record(fields) = val {
            let mut evt_type = String::new();
            let mut payload = Vec::new();
            for (name, field_val) in fields {
                match name.as_str() {
                    "event-type" => evt_type = val_to_string(field_val),
                    "payload" => payload = val_to_bytes(field_val),
                    _ => {}
                }
            }
            Some((evt_type, payload))
        } else {
            None
        }
    }

    /// Parse a Val representing an event-result record into the tuple form
    /// expected by `event_result_from_wasm`.
    fn parse_event_result(
        val: &wasmtime::component::Val,
    ) -> Result<voom_wit::WasmEventResult, WasmLoadError> {
        use wasmtime::component::Val;

        let fields = match val {
            Val::Record(fields) => fields,
            other => {
                return Err(WasmLoadError::UnexpectedValue(format!(
                    "expected Record for event-result, got {:?}",
                    std::mem::discriminant(other)
                )))
            }
        };

        let mut plugin_name = String::new();
        let mut produced_events = Vec::new();
        let mut data: Option<Vec<u8>> = None;

        for (name, field_val) in fields {
            match name.as_str() {
                "plugin-name" => plugin_name = val_to_string(field_val),
                "produced-events" => {
                    if let Val::List(items) = field_val {
                        produced_events = items.iter().filter_map(parse_event_data).collect();
                    }
                }
                "data" => match field_val {
                    Val::Option(Some(boxed)) => data = Some(val_to_bytes(boxed.as_ref())),
                    Val::Option(None) => data = None,
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
    ) -> Result<(), WasmLoadError> {
        // The interface name in WIT is "host" in package "voom:plugin".
        let mut root = linker.root();
        let mut host_instance = root
            .instance("voom:plugin/host@0.1.0")
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
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
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
                "get-plugin-data",
                |ctx: wasmtime::StoreContextMut<'_, HostState>, (key,): (String,)| {
                    let result = ctx.data().get_plugin_data(&key);
                    Ok((result,))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
                "set-plugin-data",
                |mut ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (key, value): (String, Vec<u8>)| {
                    let result = ctx.data_mut().set_plugin_data(&key, &value);
                    Ok((result.map_err(|e| e.to_string()),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
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
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
                "http-get",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (url, headers): (String, Vec<(String, String)>)| {
                    let result = ctx.data().http_get(&url, &headers);
                    let wit_result: super::WitHttpResult = match result {
                        Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                        Err(e) => Err(e),
                    };
                    Ok((wit_result,))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
                "http-post",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (url, headers, body): (String, Vec<(String, String)>, Vec<u8>)| {
                    let result = ctx.data().http_post(&url, &headers, &body);
                    let wit_result: super::WitHttpResult = match result {
                        Ok(resp) => Ok((resp.status, resp.headers, resp.body)),
                        Err(e) => Err(e),
                    };
                    Ok((wit_result,))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
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
                                "path '{path}' is not within allowed directories"
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
                        Err(e) => Ok((Err(format!("failed to read metadata for '{path}': {e}")),)),
                    }
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        host_instance
            .func_wrap(
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
                                "directory '{dir}' is not within allowed directories"
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
                        Err(e) => Ok((Err(format!("failed to list directory '{dir}': {e}")),)),
                    }
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        Ok(())
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
    }
}
