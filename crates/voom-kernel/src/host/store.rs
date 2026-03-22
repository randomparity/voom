//! Plugin data storage traits and implementations.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Trait for persistent plugin data storage.
/// Implemented by the sqlite-store plugin or in-memory for testing.
pub trait PluginDataStore: Send + Sync {
    fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String>;
    fn set(&self, plugin_name: &str, key: &str, value: &[u8]) -> Result<(), String>;
    fn delete(&self, plugin_name: &str, key: &str) -> Result<(), String>;
}

/// In-memory implementation of `PluginDataStore` for testing.
pub struct InMemoryDataStore {
    data: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl InMemoryDataStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryDataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginDataStore for InMemoryDataStore {
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

/// Adapter that implements `PluginDataStore` by delegating to `StorageTrait`.
/// Use this in production to give WASM plugins persistent data storage.
pub struct StorageBackedDataStore {
    store: Arc<dyn voom_domain::storage::StorageTrait>,
}

impl StorageBackedDataStore {
    pub fn new(store: Arc<dyn voom_domain::storage::StorageTrait>) -> Self {
        Self { store }
    }
}

impl PluginDataStore for StorageBackedDataStore {
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
