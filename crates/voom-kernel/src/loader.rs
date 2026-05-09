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
    use parking_lot::Mutex;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use voom_domain::capabilities::Capability;
    use voom_domain::errors::VoomError;
    use voom_domain::events::{Event, EventResult};

    /// A loaded plugin paired with its manifest-declared priority.
    pub type PluginWithPriority = (Arc<dyn Plugin>, i32);

    /// Interval between epoch increments (10ms).
    /// With the default deadline of 200 ticks, this gives a 2-second timeout
    /// for WASM execution (200 ticks * 10ms = 2000ms).
    const EPOCH_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

    /// Derive the plugin name from its manifest (if present) or the WASM file stem.
    fn plugin_name_from_manifest(manifest: Option<&PluginManifest>, wasm_path: &Path) -> String {
        manifest.map(|m| m.name.clone()).unwrap_or_else(|| {
            wasm_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
    }

    pub(super) struct ManifestMetadata {
        pub(super) version: String,
        pub(super) description: String,
        pub(super) author: String,
        pub(super) license: String,
        pub(super) homepage: String,
        pub(super) capabilities: Vec<Capability>,
        pub(super) handled_events: Vec<String>,
    }

    pub(super) fn manifest_metadata(manifest: Option<&PluginManifest>) -> ManifestMetadata {
        match manifest {
            Some(manifest) => ManifestMetadata {
                version: manifest.version.clone(),
                description: manifest.description.clone(),
                author: manifest.author.clone(),
                license: manifest.license.clone(),
                homepage: manifest.homepage.clone(),
                capabilities: manifest.capabilities.clone(),
                handled_events: manifest.handles_events.clone(),
            },
            None => ManifestMetadata {
                version: "0.0.0".to_string(),
                description: String::new(),
                author: String::new(),
                license: String::new(),
                homepage: String::new(),
                capabilities: Vec::new(),
                handled_events: Vec::new(),
            },
        }
    }

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

            let mut state = host_state.unwrap_or_else(|| HostState::new(plugin_name.clone()));

            if let Some(ref manifest) = manifest {
                apply_manifest_permissions(&mut state, manifest);
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
            use crate::host::{StorageBackedPluginStore, StorageBackedTransitionStore};

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

                    // Wire storage-backed stores into every plugin when available.
                    if let Some(ref s) = storage {
                        let state = host_state
                            .get_or_insert_with(|| HostState::new(plugin_name.clone()));
                        let plugin_store: Arc<dyn crate::host::WasmPluginStore> =
                            Arc::new(StorageBackedPluginStore::new(s.clone()));
                        let transition_store: Arc<dyn crate::host::WasmTransitionStore> =
                            Arc::new(StorageBackedTransitionStore::new(s.clone()));
                        state.storage = Some(plugin_store);
                        state.transition_store = Some(transition_store);
                    }

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
        capabilities: Vec<Capability>,
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
                    let result = voom_wit::event_result_from_wasm(wasm_result)
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

        // Try the namespaced interface starting from the latest version,
        // then fall back to older versions and bare exports.
        let on_event = instance
            .get_export(&mut inner.store, None, "voom:plugin/plugin@0.3.0")
            .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
            .and_then(|idx| instance.get_func(&mut inner.store, idx))
            .or_else(|| {
                instance
                    .get_export(&mut inner.store, None, "voom:plugin/plugin@0.2.0")
                    .and_then(|idx| instance.get_export(&mut inner.store, Some(&idx), "on-event"))
                    .and_then(|idx| instance.get_func(&mut inner.store, idx))
            })
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

    fn val_kind(val: &wasmtime::component::Val) -> &'static str {
        use wasmtime::component::Val;

        match val {
            Val::Bool(_) => "bool",
            Val::S8(_) => "s8",
            Val::U8(_) => "u8",
            Val::S16(_) => "s16",
            Val::U16(_) => "u16",
            Val::S32(_) => "s32",
            Val::U32(_) => "u32",
            Val::S64(_) => "s64",
            Val::U64(_) => "u64",
            Val::Float32(_) => "float32",
            Val::Float64(_) => "float64",
            Val::Char(_) => "char",
            Val::String(_) => "string",
            Val::List(_) => "list",
            Val::Record(_) => "record",
            Val::Tuple(_) => "tuple",
            Val::Variant(_, _) => "variant",
            Val::Enum(_) => "enum",
            Val::Option(_) => "option",
            Val::Result(_) => "result",
            Val::Flags(_) => "flags",
            Val::Resource(_) => "resource",
        }
    }

    fn unexpected_field_value(
        field_name: &str,
        expected: &str,
        val: &wasmtime::component::Val,
    ) -> WasmLoadError {
        WasmLoadError::UnexpectedValue(format!(
            "field '{field_name}' expected {expected}, got {}",
            val_kind(val)
        ))
    }

    fn expect_string(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<String, WasmLoadError> {
        match val {
            wasmtime::component::Val::String(value) => Ok(value.to_string()),
            other => Err(unexpected_field_value(field_name, "string", other)),
        }
    }

    fn expect_bytes(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<Vec<u8>, WasmLoadError> {
        let wasmtime::component::Val::List(items) = val else {
            return Err(unexpected_field_value(field_name, "list<u8>", val));
        };

        let mut bytes = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            match item {
                wasmtime::component::Val::U8(value) => bytes.push(*value),
                other => {
                    return Err(WasmLoadError::UnexpectedValue(format!(
                        "field '{field_name}' expected list<u8>, got {} at index {index}",
                        val_kind(other)
                    )));
                }
            }
        }
        Ok(bytes)
    }

    fn expect_optional_string(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<Option<String>, WasmLoadError> {
        match val {
            wasmtime::component::Val::Option(Some(boxed)) => {
                expect_string(field_name, boxed.as_ref()).map(Some)
            }
            wasmtime::component::Val::Option(None) => Ok(None),
            other => Err(unexpected_field_value(field_name, "option<string>", other)),
        }
    }

    fn expect_optional_bytes(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<Option<Vec<u8>>, WasmLoadError> {
        match val {
            wasmtime::component::Val::Option(Some(boxed)) => {
                expect_bytes(field_name, boxed.as_ref()).map(Some)
            }
            wasmtime::component::Val::Option(None) => Ok(None),
            other => Err(unexpected_field_value(
                field_name,
                "option<list<u8>>",
                other,
            )),
        }
    }

    /// Parse a `Val::Record` representing an event-data into (`event_type`, payload).
    fn parse_event_data(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<(String, Vec<u8>), WasmLoadError> {
        if let wasmtime::component::Val::Record(fields) = val {
            let mut evt_type = String::new();
            let mut payload = Vec::new();
            for (name, field_val) in fields {
                match name.as_str() {
                    "event-type" => {
                        evt_type = expect_string(&format!("{field_name}.event-type"), field_val)?;
                    }
                    "payload" => {
                        payload = expect_bytes(&format!("{field_name}.payload"), field_val)?;
                    }
                    _ => {}
                }
            }
            Ok((evt_type, payload))
        } else {
            Err(unexpected_field_value(field_name, "record", val))
        }
    }

    fn expect_event_data_list(
        field_name: &str,
        val: &wasmtime::component::Val,
    ) -> Result<Vec<(String, Vec<u8>)>, WasmLoadError> {
        let wasmtime::component::Val::List(items) = val else {
            return Err(unexpected_field_value(field_name, "list<event-data>", val));
        };

        items
            .iter()
            .enumerate()
            .map(|(index, item)| parse_event_data(&format!("{field_name}[{index}]"), item))
            .collect()
    }

    /// Parse a Val representing an event-result record into a
    /// [`WasmEventResult`](voom_wit::WasmEventResult).
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
        let mut claimed = false;
        let mut execution_error: Option<String> = None;
        let mut execution_detail: Option<Vec<u8>> = None;

        for (name, field_val) in fields {
            match name.as_str() {
                "plugin-name" => plugin_name = expect_string(name, field_val)?,
                "produced-events" => {
                    produced_events = expect_event_data_list(name, field_val)?;
                }
                "data" => data = expect_optional_bytes(name, field_val)?,
                "claimed" => {
                    claimed = match field_val {
                        Val::Bool(value) => *value,
                        other => return Err(unexpected_field_value(name, "bool", other)),
                    };
                }
                "execution-error" => execution_error = expect_optional_string(name, field_val)?,
                "execution-detail" => execution_detail = expect_optional_bytes(name, field_val)?,
                _ => {}
            }
        }

        Ok(voom_wit::WasmEventResult {
            plugin_name,
            produced_events,
            data,
            claimed,
            execution_error,
            execution_detail,
        })
    }

    pub(super) fn apply_manifest_permissions(state: &mut HostState, manifest: &PluginManifest) {
        state.allowed_capabilities = manifest
            .capabilities
            .iter()
            .map(|c| c.kind().to_string())
            .collect();
        state.allowed_http_domains = manifest.allowed_domains.clone();
        if let Some(ref paths) = manifest.allowed_paths {
            state.allowed_paths = paths.iter().map(|p| super::expand_tilde(p)).collect();
        }
    }

    pub(super) fn host_state_from_config(plugin_name: &str, table: &toml::Table) -> HostState {
        let config_value = match serde_json::to_value(table) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!(plugin = %plugin_name, error = %e,
                    "failed to convert plugin config to JSON; using empty config");
                serde_json::json!({})
            }
        };
        let mut state = HostState::new(plugin_name.to_string()).with_initial_config(config_value);

        if let Some(paths) = table.get("allowed_paths").and_then(toml::Value::as_array) {
            let paths = paths
                .iter()
                .filter_map(toml::Value::as_str)
                .map(super::expand_tilde)
                .collect();
            state = state.with_paths(paths);
        }

        state
    }

    /// Register host function imports in the linker.
    ///
    /// These are the functions that WASM plugins can call back into the host.
    /// Registers under both `@0.3.0` (current) and `@0.2.0` (backward compat)
    /// so plugins compiled against either version can resolve their imports.
    fn register_host_functions(
        linker: &mut wasmtime::component::Linker<HostState>,
    ) -> Result<(), WasmLoadError> {
        register_host_instance(linker, "voom:plugin/host@0.3.0")?;
        register_host_instance(linker, "voom:plugin/host@0.2.0")?;
        Ok(())
    }

    /// Type alias for the linker instance builder used by registration helpers.
    type HostLinkerInstance<'a> = wasmtime::component::LinkerInstance<'a, HostState>;

    fn register_log_func(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
        instance
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
            .map_err(|e| WasmLoadError::Linker(e.to_string()))
    }

    fn register_transition_funcs(
        instance: &mut HostLinkerInstance<'_>,
    ) -> Result<(), WasmLoadError> {
        instance
            .func_wrap(
                "get-file-transitions",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (file_id,): (String,)|
                 -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                    Ok((get_file_transitions(ctx.data(), &file_id),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        instance
            .func_wrap(
                "get-path-transitions",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (path,): (String,)|
                 -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                    Ok((ctx.data().get_path_transitions(&path),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        Ok(())
    }

    pub(super) fn get_file_transitions(
        state: &HostState,
        file_id: &str,
    ) -> Result<Vec<u8>, String> {
        let file_id = uuid::Uuid::parse_str(file_id)
            .map_err(|e| format!("invalid file ID '{file_id}': {e}"))?;
        state.get_file_transitions(&file_id)
    }

    fn register_plugin_data_funcs(
        instance: &mut HostLinkerInstance<'_>,
    ) -> Result<(), WasmLoadError> {
        instance
            .func_wrap(
                "get-plugin-data",
                |ctx: wasmtime::StoreContextMut<'_, HostState>, (key,): (String,)| {
                    let result = ctx.data().get_plugin_data(&key);
                    Ok((result,))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        instance
            .func_wrap(
                "set-plugin-data",
                |mut ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (key, value): (String, Vec<u8>)| {
                    let result = ctx.data_mut().set_plugin_data(&key, &value);
                    Ok((result.map_err(|e| e.to_string()),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        Ok(())
    }

    fn register_run_tool_func(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
        // Capability enforcement for run-tool is currently coarse-grained:
        // it checks whether the plugin has any "execute:*" capability, but
        // does not verify specific operations (e.g., "execute:transcode_video"
        // vs "execute:convert_container"). Fine-grained per-operation
        // enforcement is tracked for a future sprint.
        instance
            .func_wrap(
                "run-tool",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (tool, args, timeout_ms): (String, Vec<String>, u64)| {
                    tracing::debug!(
                        plugin = %ctx.data().plugin_name,
                        tool = %tool,
                        args = ?args,
                        "WASM plugin requesting tool execution"
                    );
                    let result = ctx.data().run_tool(&tool, &args, timeout_ms);
                    let wit_result: Result<(i32, Vec<u8>, Vec<u8>), String> = match result {
                        Ok(output) => Ok((output.exit_code, output.stdout, output.stderr)),
                        Err(e) => Err(e),
                    };
                    Ok((wit_result,))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))
    }

    fn register_http_funcs(instance: &mut HostLinkerInstance<'_>) -> Result<(), WasmLoadError> {
        instance
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

        instance
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

        Ok(())
    }

    fn register_filesystem_funcs(
        instance: &mut HostLinkerInstance<'_>,
    ) -> Result<(), WasmLoadError> {
        instance
            .func_wrap(
                "write-file",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (path, content): (String, Vec<u8>)|
                 -> Result<(Result<(), String>,), wasmtime::Error> {
                    Ok((ctx.data().write_file(&path, &content),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        instance
            .func_wrap(
                "read-file-metadata",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (path,): (String,)|
                 -> Result<(Result<Vec<u8>, String>,), wasmtime::Error> {
                    Ok((read_file_metadata(ctx.data(), &path),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        instance
            .func_wrap(
                "list-files",
                |ctx: wasmtime::StoreContextMut<'_, HostState>,
                 (dir, pattern): (String, String)|
                 -> Result<(Result<Vec<String>, String>,), wasmtime::Error> {
                    Ok((list_files(ctx.data(), &dir, &pattern),))
                },
            )
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        Ok(())
    }

    pub(super) fn read_file_metadata(state: &HostState, path: &str) -> Result<Vec<u8>, String> {
        let file_path = allowed_filesystem_path(state, path, "path")?;
        match std::fs::metadata(&file_path) {
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
                rmp_serde::to_vec(&info).map_err(|e| format!("failed to serialize metadata: {e}"))
            }
            Err(e) => Err(format!("failed to read metadata for '{path}': {e}")),
        }
    }

    pub(super) fn list_files(
        state: &HostState,
        dir: &str,
        pattern: &str,
    ) -> Result<Vec<String>, String> {
        let dir_path = allowed_filesystem_path(state, dir, "directory")?;
        match std::fs::read_dir(&dir_path) {
            Ok(entries) => collect_listed_file_names(
                entries.map(|entry| entry.map(|entry| entry.file_name())),
                pattern,
                dir,
            ),
            Err(e) => Err(format!("failed to list directory '{dir}': {e}")),
        }
    }

    fn allowed_filesystem_path(
        state: &HostState,
        path: &str,
        subject: &str,
    ) -> Result<std::path::PathBuf, String> {
        if state.allowed_paths.is_empty() {
            return Err(format!(
                "{subject} '{path}' is not within allowed directories"
            ));
        }

        let candidate = std::path::Path::new(path);
        let canonical = std::fs::canonicalize(candidate)
            .map_err(|e| format!("cannot resolve path '{path}': {e}"))?;
        let allowed = state
            .allowed_paths
            .iter()
            .any(|allowed_path| canonical.starts_with(allowed_path));
        if !allowed {
            return Err(format!(
                "{subject} '{path}' is not within allowed directories"
            ));
        }

        Ok(canonical)
    }

    pub(crate) fn collect_listed_file_names<I, E>(
        entries: I,
        pattern: &str,
        dir: &str,
    ) -> Result<Vec<String>, String>
    where
        I: IntoIterator<Item = Result<std::ffi::OsString, E>>,
        E: std::fmt::Display,
    {
        let mut files = Vec::new();
        for entry in entries {
            let file_name = entry.map_err(|e| {
                format!("failed to read directory entry while listing '{dir}': {e}")
            })?;
            if pattern.is_empty() || file_name.to_string_lossy().contains(pattern) {
                files.push(file_name.to_string_lossy().to_string());
            }
        }
        Ok(files)
    }

    fn register_host_instance(
        linker: &mut wasmtime::component::Linker<HostState>,
        instance_name: &str,
    ) -> Result<(), WasmLoadError> {
        let mut root = linker.root();
        let mut instance = root
            .instance(instance_name)
            .map_err(|e| WasmLoadError::Linker(e.to_string()))?;

        register_log_func(&mut instance)?;
        register_plugin_data_funcs(&mut instance)?;
        register_run_tool_func(&mut instance)?;
        register_http_funcs(&mut instance)?;
        register_filesystem_funcs(&mut instance)?;
        register_transition_funcs(&mut instance)?;

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

    #[cfg(test)]
    mod event_result_parse_tests {
        use super::*;
        use wasmtime::component::Val;

        fn valid_event_result() -> Val {
            Val::Record(vec![
                ("plugin-name".into(), Val::String("test-plugin".into())),
                (
                    "produced-events".into(),
                    Val::List(vec![Val::Record(vec![
                        ("event-type".into(), Val::String("metadata.enriched".into())),
                        ("payload".into(), Val::List(vec![Val::U8(1), Val::U8(2)])),
                    ])]),
                ),
                (
                    "data".into(),
                    Val::Option(Some(Box::new(Val::List(vec![
                        Val::U8(b'{'),
                        Val::U8(b'}'),
                    ])))),
                ),
                ("claimed".into(), Val::Bool(true)),
                (
                    "execution-error".into(),
                    Val::Option(Some(Box::new(Val::String("failed".into())))),
                ),
                (
                    "execution-detail".into(),
                    Val::Option(Some(Box::new(Val::List(vec![Val::U8(1), Val::U8(2)])))),
                ),
            ])
        }

        fn unexpected_value_message(
            result: Result<voom_wit::WasmEventResult, WasmLoadError>,
        ) -> String {
            match result {
                Err(WasmLoadError::UnexpectedValue(message)) => message,
                Err(other) => panic!("expected UnexpectedValue, got {other}"),
                Ok(_) => panic!("expected UnexpectedValue, got successful parse"),
            }
        }

        #[test]
        fn parse_event_result_accepts_valid_known_fields() {
            let result = parse_event_result(&valid_event_result()).unwrap();

            assert_eq!(result.plugin_name, "test-plugin");
            assert_eq!(
                result.produced_events,
                vec![("metadata.enriched".to_string(), vec![1, 2])]
            );
            assert!(result.claimed);
            assert_eq!(result.execution_error, Some("failed".to_string()));
            assert_eq!(result.execution_detail, Some(vec![1, 2]));
        }

        #[test]
        fn parse_event_result_ignores_unknown_extra_fields() {
            let Val::Record(mut fields) = valid_event_result() else {
                panic!("expected record");
            };
            fields.push(("future-field".into(), Val::Bool(false)));

            let result = parse_event_result(&Val::Record(fields)).unwrap();

            assert_eq!(result.plugin_name, "test-plugin");
        }

        #[test]
        fn parse_event_result_rejects_wrong_plugin_name_shape() {
            let Val::Record(mut fields) = valid_event_result() else {
                panic!("expected record");
            };
            fields[0] = ("plugin-name".into(), Val::Bool(false));

            let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

            assert!(message.contains("plugin-name"));
            assert!(message.contains("string"));
            assert!(message.contains("bool"));
        }

        #[test]
        fn parse_event_result_rejects_wrong_payload_item_shape() {
            let Val::Record(mut fields) = valid_event_result() else {
                panic!("expected record");
            };
            fields[1] = (
                "produced-events".into(),
                Val::List(vec![Val::Record(vec![
                    ("event-type".into(), Val::String("metadata.enriched".into())),
                    ("payload".into(), Val::List(vec![Val::Bool(false)])),
                ])]),
            );

            let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

            assert!(message.contains("produced-events[0].payload"));
            assert!(message.contains("list<u8>"));
            assert!(message.contains("index 0"));
        }

        #[test]
        fn parse_event_result_rejects_wrong_optional_data_shape() {
            let Val::Record(mut fields) = valid_event_result() else {
                panic!("expected record");
            };
            fields[2] = (
                "data".into(),
                Val::Option(Some(Box::new(Val::String("not bytes".into())))),
            );

            let message = unexpected_value_message(parse_event_result(&Val::Record(fields)));

            assert!(message.contains("data"));
            assert!(message.contains("list<u8>"));
            assert!(message.contains("string"));
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
        fn test_list_files_entry_errors_are_returned() {
            let entries = vec![
                Ok(std::ffi::OsString::from("clip-a.mkv")),
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "entry denied",
                )),
                Ok(std::ffi::OsString::from("clip-b.mkv")),
            ];

            let error = collect_listed_file_names(entries, "clip", "/media")
                .expect_err("entry errors should not be skipped");

            assert!(error.contains("failed to read directory entry"));
            assert!(error.contains("/media"));
            assert!(error.contains("entry denied"));
        }

        #[test]
        fn test_list_files_entry_filtering_preserves_matches() {
            let entries: Vec<Result<std::ffi::OsString, std::io::Error>> = vec![
                Ok(std::ffi::OsString::from("clip-a.mkv")),
                Ok(std::ffi::OsString::from("notes.txt")),
                Ok(std::ffi::OsString::from("clip-b.mkv")),
            ];

            let files = collect_listed_file_names(entries, "clip", "/media").unwrap();

            assert_eq!(files, vec!["clip-a.mkv", "clip-b.mkv"]);
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

        #[test]
        fn read_metadata_and_list_files_deny_empty_allowed_paths() {
            use crate::host::HostState;

            let dir = tempfile::tempdir().unwrap();
            let file_path = dir.path().join("movie.mkv");
            std::fs::write(&file_path, b"media").unwrap();

            let state = HostState::new("test-plugin".into());
            let metadata = read_file_metadata(&state, &file_path.to_string_lossy());
            assert!(metadata
                .unwrap_err()
                .contains("not within allowed directories"));

            let files = list_files(&state, &dir.path().to_string_lossy(), "");
            assert!(files
                .unwrap_err()
                .contains("not within allowed directories"));
        }

        #[test]
        fn read_metadata_and_list_files_allow_canonical_path() {
            use crate::host::HostState;

            let dir = tempfile::tempdir().unwrap();
            let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
            let file_path = canonical_dir.join("movie.mkv");
            let ignored_file_path = canonical_dir.join("notes.txt");
            std::fs::write(&file_path, b"media").unwrap();
            std::fs::write(&ignored_file_path, b"notes").unwrap();

            let state = HostState::new("test-plugin".into()).with_paths(vec![canonical_dir]);
            let metadata = read_file_metadata(&state, &file_path.to_string_lossy()).unwrap();
            let metadata: serde_json::Value = rmp_serde::from_slice(&metadata).unwrap();
            assert_eq!(metadata["size"], 5);
            assert_eq!(metadata["is_file"], true);

            let files = list_files(&state, &dir.path().to_string_lossy(), ".mkv").unwrap();
            assert_eq!(files, vec!["movie.mkv"]);
        }

        #[test]
        fn read_metadata_and_list_files_reject_outside_configured_path() {
            use crate::host::HostState;

            let allowed_dir = tempfile::tempdir().unwrap();
            let blocked_dir = tempfile::tempdir().unwrap();
            let blocked_file = blocked_dir.path().join("movie.mkv");
            std::fs::write(&blocked_file, b"media").unwrap();

            let allowed_dir = std::fs::canonicalize(allowed_dir.path()).unwrap();
            let state = HostState::new("test-plugin".into()).with_paths(vec![allowed_dir]);

            let metadata = read_file_metadata(&state, &blocked_file.to_string_lossy());
            assert!(metadata
                .unwrap_err()
                .contains("not within allowed directories"));

            let files = list_files(&state, &blocked_dir.path().to_string_lossy(), "");
            assert!(files
                .unwrap_err()
                .contains("not within allowed directories"));
        }

        #[test]
        fn read_metadata_rejects_missing_path_before_metadata_lookup() {
            use crate::host::HostState;

            let allowed_dir = tempfile::tempdir().unwrap();
            let allowed_dir = std::fs::canonicalize(allowed_dir.path()).unwrap();
            let state = HostState::new("test-plugin".into()).with_paths(vec![allowed_dir.clone()]);
            let missing_path = allowed_dir.join("missing.mov");

            let error = read_file_metadata(&state, &missing_path.to_string_lossy()).unwrap_err();

            assert!(
                error.contains("cannot resolve path"),
                "unexpected missing path error: {error}"
            );
        }

        #[test]
        fn get_file_transitions_rejects_malformed_uuid() {
            use crate::host::HostState;

            let state = HostState::new("test-plugin".into());
            let error = get_file_transitions(&state, "not-a-uuid").unwrap_err();

            assert!(error.contains("invalid file ID 'not-a-uuid'"));
        }

        #[test]
        fn host_state_from_config_seeds_config_and_allowed_paths() {
            let dir = tempfile::tempdir().unwrap();
            let config: toml::Table = toml::from_str(&format!(
                r#"
allowed_paths = ["{}"]
quality = "preview"
"#,
                dir.path().display()
            ))
            .expect("valid config TOML");

            let state = host_state_from_config("test-plugin", &config);

            assert_eq!(state.plugin_name, "test-plugin");
            assert_eq!(state.allowed_paths, vec![dir.path().to_path_buf()]);
            let config_bytes = state
                .plugin_data
                .get("config")
                .expect("config should be seeded");
            let config_value: serde_json::Value = serde_json::from_slice(config_bytes).unwrap();
            assert_eq!(config_value["quality"], "preview");
        }

        #[test]
        fn manifest_metadata_defaults_when_manifest_missing() {
            let metadata = manifest_metadata(None);

            assert_eq!(metadata.version, "0.0.0");
            assert!(metadata.description.is_empty());
            assert!(metadata.author.is_empty());
            assert!(metadata.license.is_empty());
            assert!(metadata.homepage.is_empty());
            assert!(metadata.capabilities.is_empty());
            assert!(metadata.handled_events.is_empty());
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

            apply_manifest_permissions(&mut state, &manifest);

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

            apply_manifest_permissions(&mut state, &manifest);

            assert_eq!(
                state.allowed_paths.len(),
                1,
                "omitted allowed_paths must preserve config-provided paths"
            );
        }

        #[test]
        fn test_manifest_allowed_paths_override_configured_paths() {
            use crate::host::HostState;
            use crate::manifest::PluginManifest;

            let config_dir = tempfile::tempdir().unwrap();
            let manifest_dir = tempfile::tempdir().unwrap();
            let config_dir = std::fs::canonicalize(config_dir.path()).unwrap();
            let manifest_dir = std::fs::canonicalize(manifest_dir.path()).unwrap();
            let mut state = HostState::new("test-plugin".into()).with_paths(vec![config_dir]);

            let manifest: PluginManifest = toml::from_str(&format!(
                r#"
name = "test-plugin"
version = "1.0.0"
description = "test"
capabilities = []
handles_events = []
allowed_paths = ["{}"]
"#,
                manifest_dir.display()
            ))
            .expect("valid manifest TOML");

            apply_manifest_permissions(&mut state, &manifest);

            assert_eq!(state.allowed_paths, vec![manifest_dir]);
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
