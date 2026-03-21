use std::path::Path;

use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::MaintenanceStorage;

use super::{escape_like, storage_err, SqliteStore};

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

        // Also prune bad_files whose paths no longer exist under root
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
            self.chunked_delete("bad_files", "id", &missing_bad_ids)?;
        }

        // Phase 1: Query file paths under root (release connection after)
        let files: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT id, path FROM files WHERE path LIKE ?1 || '%' ESCAPE '\\'")
                .map_err(storage_err("failed to prepare prune query"))?;

            let result = stmt
                .query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(storage_err("failed to query files"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err("failed to collect files"))?;
            result
        };

        // Phase 2: Check filesystem (no connection held)
        let missing_ids: Vec<&str> = files
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .map(|(id, _)| id.as_str())
            .collect();

        if missing_ids.is_empty() {
            return Ok(0);
        }

        // Phase 3: Delete dependents then files.
        // Explicit deletion of plans and processing_stats ensures cleanup works
        // on existing databases where CASCADE constraints may be missing.
        self.chunked_delete("plans", "file_id", &missing_ids)?;
        self.chunked_delete("processing_stats", "file_id", &missing_ids)?;
        let pruned = self.chunked_delete("files", "id", &missing_ids)?;

        Ok(pruned)
    }
}
