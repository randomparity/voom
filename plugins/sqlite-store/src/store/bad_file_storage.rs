use std::path::Path;

use rusqlite::params;
use uuid::Uuid;

use voom_domain::bad_file::BadFile;
use voom_domain::errors::Result;
use voom_domain::storage::{BadFileFilters, BadFileStorage};

use super::{escape_like, row_to_bad_file, storage_err, OptionalExt, SqlQuery, SqliteStore};

impl BadFileStorage for SqliteStore {
    /// Insert or update a bad file record.
    ///
    /// On conflict (same path), the existing row's `id` is preserved; the
    /// caller's `bad_file.id` is used only for the initial insert. The
    /// `attempt_count` is incremented on each subsequent upsert.
    fn upsert_bad_file(&self, bad_file: &BadFile) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO bad_files (id, path, size, content_hash, error, error_source, attempt_count, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(path) DO UPDATE SET
                 error = excluded.error,
                 error_source = excluded.error_source,
                 size = excluded.size,
                 content_hash = excluded.content_hash,
                 attempt_count = attempt_count + 1,
                 last_seen_at = excluded.last_seen_at",
            params![
                bad_file.id.to_string(),
                bad_file.path.to_string_lossy().to_string(),
                bad_file.size as i64,
                bad_file.content_hash,
                bad_file.error,
                bad_file.error_source.to_string(),
                i64::from(bad_file.attempt_count),
                bad_file.first_seen_at.to_rfc3339(),
                bad_file.last_seen_at.to_rfc3339(),
            ],
        )
        .map_err(storage_err("failed to upsert bad file"))?;
        Ok(())
    }

    fn bad_file_by_path(&self, path: &Path) -> Result<Option<BadFile>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        conn.query_row(
            "SELECT id, path, size, content_hash, error, error_source, attempt_count, first_seen_at, last_seen_at
             FROM bad_files WHERE path = ?1",
            params![path_str],
            row_to_bad_file,
        )
        .optional()
        .map_err(storage_err("failed to get bad file"))
    }

    fn list_bad_files(&self, filters: &BadFileFilters) -> Result<Vec<BadFile>> {
        let conn = self.conn()?;
        let mut q = SqlQuery::new(
            "SELECT id, path, size, content_hash, error, error_source, attempt_count, first_seen_at, last_seen_at FROM bad_files WHERE 1=1",
        );

        if let Some(ref prefix) = filters.path_prefix {
            q.condition(
                " AND path LIKE {} ESCAPE '\\'",
                format!("{}%", escape_like(prefix)),
            );
        }
        if let Some(ref source) = filters.error_source {
            q.condition(" AND error_source = {}", source.to_string());
        }

        q.sql.push_str(" ORDER BY last_seen_at DESC");

        let limit = filters.limit.unwrap_or(10_000).min(10_000);
        let offset = filters.offset.unwrap_or(0);
        q.condition(" LIMIT {}", limit.to_string());
        q.condition(" OFFSET {}", offset.to_string());

        let mut stmt = conn
            .prepare(&q.sql)
            .map_err(storage_err("failed to prepare bad files query"))?;

        let bad_files = stmt
            .query_map(q.param_refs().as_slice(), row_to_bad_file)
            .map_err(storage_err("failed to query bad files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect bad files"))?;

        Ok(bad_files)
    }

    fn count_bad_files(&self, filters: &BadFileFilters) -> Result<u64> {
        let conn = self.conn()?;
        let mut q = SqlQuery::new("SELECT COUNT(*) FROM bad_files WHERE 1=1");

        if let Some(ref prefix) = filters.path_prefix {
            q.condition(
                " AND path LIKE {} ESCAPE '\\'",
                format!("{}%", escape_like(prefix)),
            );
        }
        if let Some(ref source) = filters.error_source {
            q.condition(" AND error_source = {}", source.to_string());
        }

        let count: i64 = conn
            .query_row(&q.sql, q.param_refs().as_slice(), |row| row.get(0))
            .map_err(storage_err("failed to count bad files"))?;
        Ok(count as u64)
    }

    fn delete_bad_file(&self, id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM bad_files WHERE id = ?1",
            params![id.to_string()],
        )
        .map_err(storage_err("failed to delete bad file"))?;
        Ok(())
    }

    fn delete_bad_file_by_path(&self, path: &Path) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM bad_files WHERE path = ?1",
            params![path.to_string_lossy().to_string()],
        )
        .map_err(storage_err("failed to delete bad file by path"))?;
        Ok(())
    }
}
