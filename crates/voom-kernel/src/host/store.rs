//! Plugin data storage traits and implementations for the WASM host boundary.
//!
//! Uses `Result<T, String>` rather than `VoomError` because WIT interfaces
//! can only carry string errors across the WASM ABI boundary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Key-value store interface for WASM plugin data at the host boundary.
///
/// Uses `Result<T, String>` because WIT's `result<T, string>` ABI requires
/// string-typed errors. The [`StorageBackedPluginStore`] adapter converts
/// `VoomError` to `String` at this boundary.
pub trait WasmPluginStore: Send + Sync {
    fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String>;
}

/// In-memory implementation of [`WasmPluginStore`] for testing.
pub struct InMemoryPluginStore {
    data: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl InMemoryPluginStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryPluginStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmPluginStore for InMemoryPluginStore {
    fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        let data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        Ok(data.get(plugin_name).and_then(|m| m.get(key)).cloned())
    }

    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        data.entry(plugin_name.to_string())
            .or_default()
            .insert(key.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = data.get_mut(plugin_name) {
            m.remove(key);
        }
        Ok(())
    }
}

/// Adapter that bridges [`WasmPluginStore`] to the domain's [`StorageTrait`](voom_domain::storage::StorageTrait).
///
/// Converts `VoomError` to `String` at the WASM ABI boundary.
pub struct StorageBackedPluginStore {
    store: Arc<dyn voom_domain::storage::StorageTrait>,
}

impl StorageBackedPluginStore {
    pub fn new(store: Arc<dyn voom_domain::storage::StorageTrait>) -> Self {
        Self { store }
    }
}

impl WasmPluginStore for StorageBackedPluginStore {
    fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        self.store
            .get_plugin_data(plugin_name, key)
            .map_err(|e| e.to_string())
    }

    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String> {
        self.store
            .set_plugin_data(plugin_name, key, value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String> {
        self.store
            .delete_plugin_data(plugin_name, key)
            .map_err(|e| e.to_string())
    }
}
