//! Plugin data host functions.

use crate::host::{HostState, MAX_PLUGIN_DATA_VALUE_SIZE};

impl HostState {
    /// Resolve plugin-specific persisted data by key, using a fallback chain.
    ///
    /// Lookup order:
    /// 1. Persistent storage backend (if attached via `with_storage`)
    /// 2. In-memory `plugin_data` map (seeded by `with_initial_config`)
    ///
    /// This fallback chain means config seeded via `with_initial_config` acts as a
    /// default that the plugin can override by calling `set_plugin_data` (which
    /// writes to persistent storage). Once overridden, the storage value takes
    /// precedence on all subsequent reads.
    pub fn get_plugin_data(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        if let Some(storage) = &self.storage {
            match storage.get(&self.plugin_name, key) {
                Ok(Some(data)) => Ok(Some(data)),
                Ok(None) => Ok(self.plugin_data.get(key).cloned()),
                Err(e) => Err(format!(
                    "failed to read plugin data for plugin '{}' key '{}': {e}",
                    self.plugin_name, key
                )),
            }
        } else {
            Ok(self.plugin_data.get(key).cloned())
        }
    }

    /// Set plugin-specific persisted data.
    pub fn set_plugin_data(&mut self, key: &str, value: &[u8]) -> Result<(), String> {
        self.require_capability_kind("store", "plugin data mutation")?;
        if value.len() > MAX_PLUGIN_DATA_VALUE_SIZE {
            return Err(format!(
                "plugin data value exceeds maximum size ({} bytes, max {})",
                value.len(),
                MAX_PLUGIN_DATA_VALUE_SIZE
            ));
        }
        if let Some(storage) = &self.storage {
            storage.set(&self.plugin_name, key, value)
        } else {
            self.plugin_data.insert(key.to_string(), value.to_vec());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use crate::host::{
        HostState, InMemoryPluginStore, WasmPluginStore, MAX_PLUGIN_DATA_VALUE_SIZE,
    };

    fn state_with_capability(kind: &str) -> HostState {
        HostState::new("test".into()).with_capabilities(HashSet::from([kind.to_string()]))
    }

    #[test]
    fn test_set_plugin_data_exact_boundary_accepted() {
        let mut state = state_with_capability("store");
        let at_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE];
        assert!(state.set_plugin_data("k", &at_limit).is_ok());
    }

    #[test]
    fn test_set_plugin_data_one_over_boundary_rejected() {
        let mut state = state_with_capability("store");
        let over_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE + 1];
        let err = state.set_plugin_data("k", &over_limit).unwrap_err();
        assert!(
            err.contains("exceeds maximum size"),
            "expected size-limit rejection, got: {err}"
        );
    }

    #[test]
    fn test_set_plugin_data_size_limit_checked_before_storage() {
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = state_with_capability("store").with_storage(Arc::clone(&store));
        let over_limit = vec![0_u8; MAX_PLUGIN_DATA_VALUE_SIZE + 1];
        let err = state.set_plugin_data("k", &over_limit).unwrap_err();
        assert!(
            err.contains("exceeds maximum size"),
            "size limit must be enforced before reaching storage, got: {err}"
        );
        assert!(store.get("test", "k").unwrap().is_none());
    }

    #[test]
    fn test_set_plugin_data_writes_to_storage_when_attached() {
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("routing-plugin".into())
            .with_storage(Arc::clone(&store))
            .with_capabilities(HashSet::from(["store".to_string()]));

        state.set_plugin_data("key", b"value").unwrap();

        assert_eq!(
            store.get("routing-plugin", "key").unwrap().as_deref(),
            Some(b"value".as_ref())
        );
        assert!(!state.plugin_data.contains_key("key"));
    }

    #[test]
    fn test_get_plugin_data_falls_back_to_in_memory() {
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        let mut state = HostState::new("fallback-plugin".into()).with_storage(store);

        state
            .plugin_data
            .insert("seeded".into(), b"in-mem".to_vec());

        let got = state.get_plugin_data("seeded").unwrap();
        assert_eq!(got.as_deref(), Some(b"in-mem".as_ref()));
    }

    #[test]
    fn test_get_plugin_data_storage_hit_overrides_in_memory() {
        let store: Arc<dyn WasmPluginStore> = Arc::new(InMemoryPluginStore::new());
        store
            .set("override-plugin", "key", b"from-storage")
            .unwrap();

        let mut state = HostState::new("override-plugin".into()).with_storage(store);
        state
            .plugin_data
            .insert("key".into(), b"from-memory".to_vec());

        let got = state.get_plugin_data("key").unwrap();
        assert_eq!(got.as_deref(), Some(b"from-storage".as_ref()));
    }

    #[test]
    fn test_get_plugin_data_storage_error_does_not_fall_back() {
        struct FailingStore;

        impl WasmPluginStore for FailingStore {
            fn get(&self, plugin_name: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
                Err(format!("{plugin_name}:{key}: backend unavailable"))
            }

            fn set(&self, _plugin_name: &str, _key: &str, _value: &[u8]) -> Result<(), String> {
                Ok(())
            }

            fn delete(&self, _plugin_name: &str, _key: &str) -> Result<(), String> {
                Ok(())
            }
        }

        let mut state =
            HostState::new("failing-plugin".into()).with_storage(Arc::new(FailingStore));
        state
            .plugin_data
            .insert("config".into(), b"{\"seeded\":true}".to_vec());

        let err = state.get_plugin_data("config").unwrap_err();
        assert!(err.contains("failed to read plugin data"));
        assert!(err.contains("backend unavailable"));
    }
}
