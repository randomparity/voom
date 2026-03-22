use chrono::Utc;
use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::PluginDataStorage;

use super::{format_datetime, storage_err, OptionalExt, SqliteStore};

impl PluginDataStorage for SqliteStore {
    fn get_plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>> {
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
