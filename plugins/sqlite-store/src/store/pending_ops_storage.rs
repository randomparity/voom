use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::storage::PendingOperation;

use super::{format_datetime, other_storage_err, storage_err, SqliteStore};

impl voom_domain::storage::PendingOpsStorage for SqliteStore {
    fn insert_pending_op(&self, op: &PendingOperation) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO pending_operations \
             (id, file_path, phase_name, started_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                op.id.to_string(),
                op.file_path.to_string_lossy().to_string(),
                op.phase_name,
                format_datetime(&op.started_at),
            ],
        )
        .map_err(storage_err("failed to insert pending operation"))?;
        Ok(())
    }

    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM pending_operations WHERE id = ?1",
            params![plan_id.to_string()],
        )
        .map_err(storage_err("failed to delete pending operation"))?;
        Ok(())
    }

    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, phase_name, started_at \
                 FROM pending_operations ORDER BY started_at",
            )
            .map_err(storage_err("failed to prepare pending ops query"))?;

        let ops = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let file_path: String = row.get(1)?;
                let phase_name: String = row.get(2)?;
                let started_at_str: String = row.get(3)?;
                Ok((id_str, file_path, phase_name, started_at_str))
            })
            .map_err(storage_err("failed to query pending ops"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect pending ops"))?;

        let mut result = Vec::with_capacity(ops.len());
        for (id_str, file_path, phase_name, started_at_str) in ops {
            let id = super::parse_uuid(&id_str)?;
            let started_at = started_at_str.parse().map_err(other_storage_err(&format!(
                "corrupt datetime in pending_operations: {started_at_str}"
            )))?;
            result.push(PendingOperation {
                id,
                file_path: std::path::PathBuf::from(file_path),
                phase_name,
                started_at,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;
    use voom_domain::storage::{PendingOperation, PendingOpsStorage};

    use crate::store::SqliteStore;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    fn sample_op() -> PendingOperation {
        PendingOperation {
            id: Uuid::new_v4(),
            file_path: std::path::PathBuf::from("/media/movies/test.mkv"),
            phase_name: "normalize".to_string(),
            started_at: Utc::now(),
        }
    }

    #[test]
    fn test_insert_and_list_pending_op() {
        let store = test_store();
        let op = sample_op();
        store.insert_pending_op(&op).unwrap();

        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, op.id);
        assert_eq!(ops[0].file_path, op.file_path);
        assert_eq!(ops[0].phase_name, op.phase_name);
    }

    #[test]
    fn test_list_empty_pending_ops() {
        let store = test_store();
        let ops = store.list_pending_ops().unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_delete_pending_op() {
        let store = test_store();
        let op = sample_op();
        store.insert_pending_op(&op).unwrap();

        store.delete_pending_op(&op.id).unwrap();

        let ops = store.list_pending_ops().unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_delete_nonexistent_pending_op_is_noop() {
        let store = test_store();
        let fake_id = Uuid::new_v4();
        // Should not error
        store.delete_pending_op(&fake_id).unwrap();
    }

    #[test]
    fn test_insert_or_replace_pending_op() {
        let store = test_store();
        let op = sample_op();
        store.insert_pending_op(&op).unwrap();

        // Insert with same id but different phase_name — should replace
        let updated = PendingOperation {
            id: op.id,
            file_path: std::path::PathBuf::from("/media/movies/test.mkv"),
            phase_name: "transcode".to_string(),
            started_at: op.started_at,
        };
        store.insert_pending_op(&updated).unwrap();

        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].phase_name, "transcode");
    }

    #[test]
    fn test_list_pending_ops_ordered_by_started_at() {
        let store = test_store();

        let t1 = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let t2 = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let op2 = PendingOperation {
            id: Uuid::new_v4(),
            file_path: std::path::PathBuf::from("/media/b.mkv"),
            phase_name: "normalize".to_string(),
            started_at: t2,
        };
        let op1 = PendingOperation {
            id: Uuid::new_v4(),
            file_path: std::path::PathBuf::from("/media/a.mkv"),
            phase_name: "normalize".to_string(),
            started_at: t1,
        };

        // Insert in reverse order
        store.insert_pending_op(&op2).unwrap();
        store.insert_pending_op(&op1).unwrap();

        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].started_at, t1);
        assert_eq!(ops[1].started_at, t2);
    }

    #[test]
    fn test_multiple_pending_ops_different_files() {
        let store = test_store();
        let op1 = PendingOperation {
            id: Uuid::new_v4(),
            file_path: std::path::PathBuf::from("/media/movies/a.mkv"),
            phase_name: "normalize".to_string(),
            started_at: Utc::now(),
        };
        let op2 = PendingOperation {
            id: Uuid::new_v4(),
            file_path: std::path::PathBuf::from("/media/movies/b.mkv"),
            phase_name: "transcode".to_string(),
            started_at: Utc::now(),
        };

        store.insert_pending_op(&op1).unwrap();
        store.insert_pending_op(&op2).unwrap();

        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 2);

        store.delete_pending_op(&op1.id).unwrap();
        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, op2.id);
    }
}
