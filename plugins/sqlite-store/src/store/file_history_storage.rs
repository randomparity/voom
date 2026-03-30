use std::path::{Path, PathBuf};

use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::media::Container;
use voom_domain::storage::FileHistoryStorage;

use super::{parse_optional_datetime, row_uuid, storage_err, SqliteStore};

impl FileHistoryStorage for SqliteStore {
    fn file_history(&self, path: &Path) -> Result<Vec<voom_domain::storage::FileHistoryEntry>> {
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
                let container_str: String = row.get("container")?;
                let introspected_str: String = row.get("introspected_at")?;
                let archived_str: String = row.get("archived_at")?;
                Ok(voom_domain::storage::FileHistoryEntry::from_stored(
                    voom_domain::storage::StoredHistoryRow::new(
                        row_uuid(&id_str, "file_history")?,
                        row_uuid(&file_id_str, "file_history")?,
                        PathBuf::from(row.get::<_, String>("path")?),
                        {
                            let h: String = row.get("content_hash")?;
                            if h.is_empty() {
                                None
                            } else {
                                Some(h)
                            }
                        },
                        Container::from_extension(&container_str),
                        u32::try_from(row.get::<_, i32>("track_count")?).unwrap_or(0),
                        parse_optional_datetime(
                            Some(introspected_str),
                            "file_history.introspected_at",
                        )?
                        .unwrap_or_default(),
                        parse_optional_datetime(Some(archived_str), "file_history.archived_at")?
                            .unwrap_or_default(),
                    ),
                ))
            })
            .map_err(storage_err("failed to query history"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect history"))?;

        Ok(entries)
    }
}
