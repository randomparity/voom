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

/// Transition query interface for the WASM host boundary.
///
/// Uses `Result<T, String>` because WIT's `result<T, string>` ABI requires
/// string-typed errors (same rationale as [`WasmPluginStore`]).
pub trait WasmTransitionStore: Send + Sync {
    fn transitions_for_file(
        &self,
        file_id: &uuid::Uuid,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String>;

    fn transitions_for_path(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String>;
}

/// In-memory implementation of [`WasmTransitionStore`] for testing.
pub struct InMemoryTransitionStore {
    transitions: Mutex<Vec<voom_domain::transition::FileTransition>>,
}

impl InMemoryTransitionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            transitions: Mutex::new(Vec::new()),
        }
    }

    /// Record a transition (test helper — not part of the `WasmTransitionStore` trait).
    #[cfg(test)]
    pub fn record_transition(
        &self,
        transition: &voom_domain::transition::FileTransition,
    ) -> Result<(), String> {
        self.transitions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(transition.clone());
        Ok(())
    }
}

impl Default for InMemoryTransitionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmTransitionStore for InMemoryTransitionStore {
    fn transitions_for_file(
        &self,
        file_id: &uuid::Uuid,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String> {
        let data = self.transitions.lock().unwrap_or_else(|e| e.into_inner());
        Ok(data
            .iter()
            .filter(|t| t.file_id == *file_id)
            .cloned()
            .collect())
    }

    fn transitions_for_path(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String> {
        let data = self.transitions.lock().unwrap_or_else(|e| e.into_inner());
        Ok(data.iter().filter(|t| t.path == path).cloned().collect())
    }
}

/// Adapter bridging [`WasmTransitionStore`] to the domain's
/// [`FileTransitionStorage`](voom_domain::storage::FileTransitionStorage).
pub struct StorageBackedTransitionStore {
    store: Arc<dyn voom_domain::storage::StorageTrait>,
}

impl StorageBackedTransitionStore {
    #[must_use]
    pub fn new(store: Arc<dyn voom_domain::storage::StorageTrait>) -> Self {
        Self { store }
    }
}

impl WasmTransitionStore for StorageBackedTransitionStore {
    fn transitions_for_file(
        &self,
        file_id: &uuid::Uuid,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String> {
        self.store
            .transitions_for_file(file_id)
            .map_err(|e| e.to_string())
    }

    fn transitions_for_path(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<voom_domain::transition::FileTransition>, String> {
        self.store
            .transitions_for_path(path)
            .map_err(|e| e.to_string())
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
            .plugin_data(plugin_name, key)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_transition_store_empty() {
        let store = InMemoryTransitionStore::new();
        let id = uuid::Uuid::new_v4();
        assert!(store.transitions_for_file(&id).unwrap().is_empty());
    }

    #[test]
    fn test_in_memory_transition_store_roundtrip() {
        use std::path::PathBuf;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = InMemoryTransitionStore::new();
        let file_id = uuid::Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/movies/test.mkv"),
            "hash123".into(),
            2000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t).unwrap();

        let results = store.transitions_for_file(&file_id).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].to_hash, "hash123");
    }

    #[test]
    fn test_in_memory_transition_store_by_path() {
        use std::path::PathBuf;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = InMemoryTransitionStore::new();
        let path = PathBuf::from("/movies/test.mkv");
        let t = FileTransition::new(
            uuid::Uuid::new_v4(),
            path.clone(),
            "hash123".into(),
            2000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t).unwrap();

        let results = store.transitions_for_path(&path).unwrap();
        assert_eq!(results.len(), 1);

        let empty = store
            .transitions_for_path(&PathBuf::from("/other.mkv"))
            .unwrap();
        assert!(empty.is_empty());
    }

    fn open_sqlite_store() -> Arc<dyn voom_domain::storage::StorageTrait> {
        Arc::new(
            voom_sqlite_store::store::SqliteStore::in_memory()
                .expect("open in-memory SQLite store"),
        )
    }

    #[test]
    fn test_storage_backed_plugin_store_roundtrip() {
        let store = open_sqlite_store();
        let adapter = StorageBackedPluginStore::new(store);

        // Initially empty
        assert!(adapter.get("my-plugin", "key1").unwrap().is_none());

        // Write and read back
        adapter.set("my-plugin", "key1", b"hello").unwrap();
        let val = adapter.get("my-plugin", "key1").unwrap();
        assert_eq!(val.as_deref(), Some(b"hello".as_ref()));

        // Overwrite
        adapter.set("my-plugin", "key1", b"world").unwrap();
        let val = adapter.get("my-plugin", "key1").unwrap();
        assert_eq!(val.as_deref(), Some(b"world".as_ref()));

        // Delete
        adapter.delete("my-plugin", "key1").unwrap();
        assert!(adapter.get("my-plugin", "key1").unwrap().is_none());
    }

    #[test]
    fn test_storage_backed_plugin_store_namespace_isolation() {
        let store = open_sqlite_store();
        let adapter = StorageBackedPluginStore::new(store);

        adapter.set("plugin-a", "key", b"aaa").unwrap();
        adapter.set("plugin-b", "key", b"bbb").unwrap();

        assert_eq!(
            adapter.get("plugin-a", "key").unwrap().as_deref(),
            Some(b"aaa".as_ref())
        );
        assert_eq!(
            adapter.get("plugin-b", "key").unwrap().as_deref(),
            Some(b"bbb".as_ref())
        );
    }

    #[test]
    fn test_storage_backed_transition_store_roundtrip() {
        use std::path::PathBuf;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = open_sqlite_store();
        let file_id = uuid::Uuid::new_v4();
        let path = PathBuf::from("/movies/test.mkv");
        let t = FileTransition::new(
            file_id,
            path.clone(),
            "hash789".into(),
            5000,
            TransitionSource::Voom,
        );
        store.record_transition(&t).expect("record transition");

        let adapter = StorageBackedTransitionStore::new(store);

        // Query by file ID
        let by_id = adapter.transitions_for_file(&file_id).unwrap();
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].to_hash, "hash789");

        // Query by path
        let by_path = adapter.transitions_for_path(&path).unwrap();
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].to_hash, "hash789");

        // Non-existent path returns empty
        let empty = adapter
            .transitions_for_path(&PathBuf::from("/other.mkv"))
            .unwrap();
        assert!(empty.is_empty());
    }
}
