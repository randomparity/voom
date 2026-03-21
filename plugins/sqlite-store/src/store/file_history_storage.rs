use std::path::{Path, PathBuf};

use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::FileHistoryStorage;

use super::{row_uuid, storage_err, SqliteStore};

impl FileHistoryStorage for SqliteStore {
    fn get_file_history(&self, path: &Path) -> Result<Vec<voom_domain::storage::FileHistoryEntry>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, content_hash, container, track_count, introspected_at, archived_at
                 FROM file_history WHERE path = ?1 ORDER BY archived_at",
            )
            .map_err(storage_err("failed to prepare history query"))?;

        let entries = stmt
            .query_map(params![path_str], |row| {
                let id_str: String = row.get("id")?;
                let file_id_str: String = row.get("file_id")?;
                Ok(voom_domain::storage::FileHistoryEntry {
                    id: row_uuid(&id_str, "file_history")?,
                    file_id: row_uuid(&file_id_str, "file_history")?,
                    path: PathBuf::from(row.get::<_, String>("path")?),
                    content_hash: row.get("content_hash")?,
                    container: row.get("container")?,
                    track_count: u32::try_from(row.get::<_, i32>("track_count")?).unwrap_or(0),
                    introspected_at: row.get("introspected_at")?,
                    archived_at: row.get("archived_at")?,
                })
            })
            .map_err(storage_err("failed to query history"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect history"))?;

        Ok(entries)
    }
}
