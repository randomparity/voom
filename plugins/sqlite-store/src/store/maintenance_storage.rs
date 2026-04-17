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

    #[test]
    fn table_row_counts_reflect_inserts() {
        use voom_domain::storage::PluginDataStorage;

        let store = test_store();

        let before = store.table_row_counts().unwrap();
        let before_plugin_data = before
            .iter()
            .find(|(t, _)| t == "plugin_data")
            .map(|(_, c)| *c)
            .unwrap();

        store.set_plugin_data("plugin", "key", b"value").unwrap();
        let after = store.table_row_counts().unwrap();
        let after_plugin_data = after
            .iter()
            .find(|(t, _)| t == "plugin_data")
            .map(|(_, c)| *c)
            .unwrap();

        assert_eq!(after_plugin_data, before_plugin_data + 1);
    }

    #[test]
    fn vacuum_on_empty_db() {
        let store = test_store();
        store.vacuum().unwrap();
    }

    #[test]
    fn vacuum_on_populated_db() {
        use voom_domain::storage::PluginDataStorage;

        let store = test_store();
        for i in 0..5 {
            store
                .set_plugin_data("p", &format!("key-{i}"), b"some-bytes")
                .unwrap();
        }
        store.vacuum().unwrap();
    }

    #[test]
    fn page_stats_returns_positive_values() {
        let store = test_store();
        let stats = store.page_stats().unwrap();
        assert!(stats.page_size > 0, "page_size must be positive");
        assert!(stats.page_count > 0, "page_count must be positive");
    }

    #[test]
    fn prune_missing_files_under_soft_deletes_absent_files() {
        use voom_domain::media::MediaFile;
        use voom_domain::storage::FileStorage;
        use voom_domain::transition::FileStatus;

        let store = test_store();
        // Insert a file with a path that cannot exist on the host filesystem.
        let mut file = MediaFile::new(std::path::PathBuf::from(
            "/definitely-not-a-real-root/ghost.mkv",
        ));
        file.content_hash = Some("h".into());
        store.upsert_file(&file).unwrap();

        let marked = store
            .prune_missing_files_under(Path::new("/definitely-not-a-real-root"))
            .unwrap();
        assert_eq!(marked, 1);
        let after = store.file(&file.id).unwrap().unwrap();
        assert_eq!(after.status, FileStatus::Missing);
    }

    #[test]
    fn prune_missing_files_under_hard_deletes_bad_files() {
        use voom_domain::bad_file::{BadFile, BadFileSource};
        use voom_domain::storage::BadFileStorage;

        let store = test_store();
        let bf = BadFile::new(
            std::path::PathBuf::from("/nope/never/there.mkv"),
            128,
            None,
            "io error".into(),
            BadFileSource::Io,
        );
        store.upsert_bad_file(&bf).unwrap();

        let _ = store.prune_missing_files_under(Path::new("/nope")).unwrap();
        // bad_files under the root that don't exist on disk should be hard-deleted.
        let remaining = store
            .bad_file_by_path(Path::new("/nope/never/there.mkv"))
            .unwrap();
        assert!(remaining.is_none());
    }
}
