use std::path::Path;

use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::{MaintenanceStorage, PageStats};

use super::{escape_like, storage_err, PruneTarget, SqliteStore};

impl MaintenanceStorage for SqliteStore {
    fn vacuum(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch("VACUUM")
            .map_err(storage_err("failed to vacuum"))?;
        Ok(())
    }

    fn prune_missing_files(&self) -> Result<u64> {
        self.prune_missing_files_under(Path::new("/"))
    }

    fn prune_missing_files_under(&self, root: &Path) -> Result<u64> {
        let root_str = escape_like(&root.to_string_lossy());

        // Hard-delete bad_files whose paths no longer exist under root.
        // bad_files don't have lifecycle tracking.
        {
            let bad_files: Vec<(String, String)> = {
                let conn = self.conn()?;
                let mut stmt = conn
                    .prepare("SELECT id, path FROM bad_files WHERE path LIKE ?1 || '%' ESCAPE '\\'")
                    .map_err(storage_err("failed to prepare bad_files prune"))?;
                let result = stmt
                    .query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(storage_err("failed to query bad_files"))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(storage_err("failed to collect bad_files"))?;
                result
            };
            let missing_bad_ids: Vec<&str> = bad_files
                .iter()
                .filter(|(_, path)| !Path::new(path).exists())
                .map(|(id, _)| id.as_str())
                .collect();
            self.chunked_delete(PruneTarget::BadFiles, &missing_bad_ids)?;
        }

        // Phase 1: Query active files under root (release connection after)
        let files: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT id, path FROM files WHERE status = 'active' AND path LIKE ?1 || '%' ESCAPE '\\'")
                .map_err(storage_err("failed to prepare prune query"))?;

            let result = stmt
                .query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(storage_err("failed to query files"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err("failed to collect files"))?;
            result
        };

        // Phase 2: Check filesystem (no connection held)
        let missing_ids: Vec<uuid::Uuid> = files
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .filter_map(|(id, _)| uuid::Uuid::parse_str(id).ok())
            .collect();

        if missing_ids.is_empty() {
            return Ok(0);
        }

        // Phase 3: Mark missing files with soft-delete.
        // Lifecycle-based purge is done separately via purge_missing() on FileStorage.
        let conn = self.conn()?;
        let now = super::format_datetime(&chrono::Utc::now());
        let mut marked = 0u64;
        for id in &missing_ids {
            conn.execute(
                "UPDATE files SET status = 'missing', missing_since = ?1 WHERE id = ?2 AND status = 'active'",
                params![&now, id.to_string()],
            )
            .map_err(storage_err("failed to mark file missing"))?;
            marked += 1;
        }

        Ok(marked)
    }

    fn table_row_counts(&self) -> Result<Vec<(String, u64)>> {
        let tables = [
            "files",
            "tracks",
            "subtitles",
            "jobs",
            "plans",
            "file_transitions",
            "plugin_data",
            "bad_files",
            "discovered_files",
            "health_checks",
            "event_log",
            "library_snapshots",
            "pending_operations",
        ];
        let conn = self.conn()?;
        let mut counts = Vec::with_capacity(tables.len());
        for table in tables {
            let count: u64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(storage_err(&format!("failed to count rows in {table}")))?;
            counts.push((table.to_string(), count));
        }
        Ok(counts)
    }

    fn page_stats(&self) -> Result<PageStats> {
        let conn = self.conn()?;
        let page_size: u64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(storage_err("failed to read page_size"))?;
        let page_count: u64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(storage_err("failed to read page_count"))?;
        let freelist_count: u64 = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .map_err(storage_err("failed to read freelist_count"))?;
        Ok(PageStats {
            page_size,
            page_count,
            freelist_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn test_table_row_counts_includes_all_tables() {
        let store = test_store();
        let counts = store.table_row_counts().expect("row counts");
        let table_names: Vec<&str> = counts.iter().map(|(name, _)| name.as_str()).collect();
        for expected in &[
            "files",
            "tracks",
            "subtitles",
            "jobs",
            "plans",
            "file_transitions",
            "plugin_data",
            "bad_files",
            "discovered_files",
            "health_checks",
            "event_log",
            "library_snapshots",
            "pending_operations",
        ] {
            assert!(table_names.contains(expected), "missing table: {expected}");
        }
    }
}
