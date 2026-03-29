//! CRUD operations for the `discovered_files` staging table.

use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;

use super::{format_datetime, parse_optional_datetime, storage_err, SqliteStore};

/// Status of a discovered file in the staging pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveredStatus {
    Pending,
    Introspecting,
    Completed,
    Failed,
}

impl DiscoveredStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Introspecting => "introspecting",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "introspecting" => Some(Self::Introspecting),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// A row from the `discovered_files` table.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub id: Uuid,
    pub path: String,
    pub size: u64,
    pub content_hash: String,
    pub status: DiscoveredStatus,
    pub discovered_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SqliteStore {
    /// Insert or update a discovered file. On conflict (same path),
    /// update size, hash, status, and timestamps.
    pub fn upsert_discovered_file(&self, path: &str, size: u64, content_hash: &str) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&chrono::Utc::now());
        let id = Uuid::new_v4().to_string();

        conn.execute(
            "INSERT INTO discovered_files (id, path, size, content_hash, status, discovered_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)
             ON CONFLICT(path) DO UPDATE SET
                size = excluded.size,
                content_hash = excluded.content_hash,
                status = 'pending',
                updated_at = excluded.updated_at",
            params![id, path, size as i64, content_hash, now],
        )
        .map_err(storage_err("failed to upsert discovered file"))?;

        Ok(())
    }

    /// Update the status of a discovered file by path.
    pub fn update_discovered_status(&self, path: &str, status: DiscoveredStatus) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&chrono::Utc::now());

        conn.execute(
            "UPDATE discovered_files SET status = ?1, updated_at = ?2 WHERE path = ?3",
            params![status.as_str(), now, path],
        )
        .map_err(storage_err("failed to update discovered file status"))?;

        Ok(())
    }

    /// List discovered files, optionally filtered by status.
    pub fn list_discovered_files(
        &self,
        status_filter: Option<DiscoveredStatus>,
    ) -> Result<Vec<DiscoveredFile>> {
        let conn = self.conn()?;

        let (sql, param_value);
        let param_refs: Vec<&dyn rusqlite::types::ToSql>;

        if let Some(status) = status_filter {
            sql = "SELECT id, path, size, content_hash, status, discovered_at, updated_at \
                   FROM discovered_files WHERE status = ?1 ORDER BY discovered_at";
            param_value = status.as_str().to_string();
            param_refs = vec![&param_value as &dyn rusqlite::types::ToSql];
        } else {
            sql = "SELECT id, path, size, content_hash, status, discovered_at, updated_at \
                   FROM discovered_files ORDER BY discovered_at";
            param_refs = vec![];
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(storage_err("failed to prepare discovered files query"))?;

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id_str: String = row.get("id")?;
                let status_str: String = row.get("status")?;
                let id = Uuid::parse_str(&id_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                let discovered_str: String = row.get("discovered_at")?;
                let updated_str: String = row.get("updated_at")?;
                Ok(DiscoveredFile {
                    id,
                    path: row.get("path")?,
                    size: row.get::<_, i64>("size")? as u64,
                    content_hash: row.get("content_hash")?,
                    status: DiscoveredStatus::parse(&status_str).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            format!("unknown discovered file status: {status_str}").into(),
                        )
                    })?,
                    discovered_at: parse_optional_datetime(
                        Some(discovered_str),
                        "discovered_files.discovered_at",
                    )?
                    .unwrap_or_default(),
                    updated_at: parse_optional_datetime(
                        Some(updated_str),
                        "discovered_files.updated_at",
                    )?
                    .unwrap_or_default(),
                })
            })
            .map_err(storage_err("failed to query discovered files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect discovered files"))?;

        Ok(rows)
    }

    /// Delete a discovered file by path (cleanup after successful introspection).
    pub fn delete_discovered_file(&self, path: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM discovered_files WHERE path = ?1",
            params![path],
        )
        .map_err(storage_err("failed to delete discovered file"))?;
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
    fn test_upsert_and_list_discovered() {
        let store = test_store();
        store
            .upsert_discovered_file("/media/test.mkv", 1024, "abc123")
            .unwrap();

        let files = store.list_discovered_files(None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "/media/test.mkv");
        assert_eq!(files[0].size, 1024);
        assert_eq!(files[0].status, DiscoveredStatus::Pending);
    }

    #[test]
    fn test_upsert_updates_existing() {
        let store = test_store();
        store
            .upsert_discovered_file("/media/test.mkv", 1024, "abc")
            .unwrap();
        store
            .upsert_discovered_file("/media/test.mkv", 2048, "def")
            .unwrap();

        let files = store.list_discovered_files(None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, 2048);
        assert_eq!(files[0].content_hash, "def");
    }

    #[test]
    fn test_update_status() {
        let store = test_store();
        store
            .upsert_discovered_file("/media/test.mkv", 1024, "abc")
            .unwrap();

        store
            .update_discovered_status("/media/test.mkv", DiscoveredStatus::Introspecting)
            .unwrap();

        let files = store
            .list_discovered_files(Some(DiscoveredStatus::Introspecting))
            .unwrap();
        assert_eq!(files.len(), 1);

        let pending = store
            .list_discovered_files(Some(DiscoveredStatus::Pending))
            .unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_delete_discovered() {
        let store = test_store();
        store
            .upsert_discovered_file("/media/test.mkv", 1024, "abc")
            .unwrap();

        store.delete_discovered_file("/media/test.mkv").unwrap();

        let files = store.list_discovered_files(None).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_list_with_status_filter() {
        let store = test_store();
        store
            .upsert_discovered_file("/media/a.mkv", 100, "aaa")
            .unwrap();
        store
            .upsert_discovered_file("/media/b.mkv", 200, "bbb")
            .unwrap();

        store
            .update_discovered_status("/media/a.mkv", DiscoveredStatus::Completed)
            .unwrap();

        let completed = store
            .list_discovered_files(Some(DiscoveredStatus::Completed))
            .unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].path, "/media/a.mkv");

        let pending = store
            .list_discovered_files(Some(DiscoveredStatus::Pending))
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "/media/b.mkv");
    }
}
