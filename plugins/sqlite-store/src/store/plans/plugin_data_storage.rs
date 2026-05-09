use chrono::Utc;
use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::PluginDataStorage;

use crate::store::{format_datetime, storage_err, OptionalExt, SqliteStore};

impl PluginDataStorage for SqliteStore {
    fn plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT value FROM plugin_data WHERE plugin_name = ?1 AND key = ?2",
            params![plugin, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_err("failed to get plugin data"))
    }

    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO plugin_data (plugin_name, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(plugin_name, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![plugin, key, value, format_datetime(&Utc::now())],
        )
        .map_err(storage_err("failed to set plugin data"))?;
        Ok(())
    }

    fn delete_plugin_data(&self, plugin: &str, key: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM plugin_data WHERE plugin_name = ?1 AND key = ?2",
            params![plugin, key],
        )
        .map_err(storage_err("failed to delete plugin data"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn set_and_get_roundtrip() {
        let store = test_store();
        store
            .set_plugin_data("my-plugin", "cache-key", b"hello world")
            .unwrap();
        let value = store
            .plugin_data("my-plugin", "cache-key")
            .unwrap()
            .unwrap();
        assert_eq!(value, b"hello world");
    }

    #[test]
    fn missing_key_returns_none() {
        let store = test_store();
        let value = store.plugin_data("some-plugin", "no-such-key").unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn overwrites_existing_value() {
        let store = test_store();
        store.set_plugin_data("plugin", "key", b"first").unwrap();
        store.set_plugin_data("plugin", "key", b"second").unwrap();
        let value = store.plugin_data("plugin", "key").unwrap().unwrap();
        assert_eq!(value, b"second");
    }

    #[test]
    fn delete_removes_value() {
        let store = test_store();
        store
            .set_plugin_data("plugin", "key", b"to-delete")
            .unwrap();
        store.delete_plugin_data("plugin", "key").unwrap();
        assert!(store.plugin_data("plugin", "key").unwrap().is_none());
    }

    #[test]
    fn delete_missing_is_noop() {
        let store = test_store();
        store.delete_plugin_data("plugin", "never-existed").unwrap();
    }

    #[test]
    fn plugins_have_isolated_namespaces() {
        let store = test_store();
        store
            .set_plugin_data("plugin-a", "shared-key", b"value-a")
            .unwrap();
        store
            .set_plugin_data("plugin-b", "shared-key", b"value-b")
            .unwrap();

        assert_eq!(
            store
                .plugin_data("plugin-a", "shared-key")
                .unwrap()
                .unwrap(),
            b"value-a"
        );
        assert_eq!(
            store
                .plugin_data("plugin-b", "shared-key")
                .unwrap()
                .unwrap(),
            b"value-b"
        );

        // Deleting one does not affect the other.
        store.delete_plugin_data("plugin-a", "shared-key").unwrap();
        assert!(store
            .plugin_data("plugin-a", "shared-key")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .plugin_data("plugin-b", "shared-key")
                .unwrap()
                .unwrap(),
            b"value-b"
        );
    }

    #[test]
    fn binary_bytes_0_to_255_roundtrip() {
        let store = test_store();
        let bytes: Vec<u8> = (0u8..=255).collect();
        store.set_plugin_data("p", "bin", &bytes).unwrap();
        let loaded = store.plugin_data("p", "bin").unwrap().unwrap();
        assert_eq!(loaded, bytes);
    }
}
