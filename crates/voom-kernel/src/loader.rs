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

/// WASM plugin loader using wasmtime.
/// Only available with the `wasm` feature.
#[cfg(feature = "wasm")]
pub mod wasm {
    use super::*;
    use std::path::Path;
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult};

    pub struct WasmPluginLoader {
        engine: wasmtime::Engine,
    }

    impl WasmPluginLoader {
        pub fn new() -> Result<Self> {
            let engine = wasmtime::Engine::default();
            Ok(Self { engine })
        }

        /// Load a .wasm plugin from the given path.
        pub fn load(&self, path: &Path) -> Result<Arc<dyn Plugin>> {
            let wasm_bytes = std::fs::read(path).map_err(|e| VoomError::Wasm(e.to_string()))?;
            let module = wasmtime::Module::new(&self.engine, &wasm_bytes)
                .map_err(|e| VoomError::Wasm(e.to_string()))?;

            Ok(Arc::new(WasmPlugin {
                name: path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                module,
                engine: self.engine.clone(),
            }))
        }
    }

    struct WasmPlugin {
        name: String,
        #[allow(dead_code)]
        module: wasmtime::Module,
        #[allow(dead_code)]
        engine: wasmtime::Engine,
    }

    // SAFETY: wasmtime types are Send + Sync
    unsafe impl Send for WasmPlugin {}
    unsafe impl Sync for WasmPlugin {}

    impl Plugin for WasmPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> &str {
            "0.0.0" // TODO: Extract from WASM module exports
        }

        fn capabilities(&self) -> &[Capability] {
            &[] // TODO: Query WASM module for capabilities via WIT
        }

        fn handles(&self, _event_type: &str) -> bool {
            false // TODO: Query WASM module via WIT
        }

        fn on_event(&self, _event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            // TODO: Serialize event, call WASM function, deserialize result
            Ok(None)
        }
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
}
