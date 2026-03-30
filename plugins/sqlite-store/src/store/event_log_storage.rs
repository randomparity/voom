use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::{EventLogFilters, EventLogRecord, EventLogStorage};

use super::{format_datetime, storage_err, SqliteStore};

impl EventLogStorage for SqliteStore {
    fn insert_event_log(&self, record: &EventLogRecord) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO event_log (id, event_type, payload, summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.id.to_string(),
                record.event_type,
                record.payload,
                record.summary,
                format_datetime(&record.created_at),
            ],
        )
        .map_err(storage_err("failed to insert event log"))?;
        let rowid = conn.last_insert_rowid();
        Ok(rowid)
    }

    fn list_event_log(&self, filters: &EventLogFilters) -> Result<Vec<EventLogRecord>> {
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT rowid, id, event_type, payload, summary, created_at
             FROM event_log WHERE 1=1",
        );
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref event_type) = filters.event_type {
            if let Some(prefix) = event_type.strip_suffix('*') {
                let escaped = super::escape_like(prefix);
                param_values.push(format!("{escaped}%"));
                sql.push_str(&format!(
                    " AND event_type LIKE ?{} ESCAPE '\\'",
                    param_values.len()
                ));
            } else {
                param_values.push(event_type.clone());
                sql.push_str(&format!(" AND event_type = ?{}", param_values.len()));
            }
        }

        if let Some(since_rowid) = filters.since_rowid {
            param_values.push(since_rowid.to_string());
            sql.push_str(&format!(" AND rowid > ?{}", param_values.len()));
        }

        sql.push_str(" ORDER BY rowid ASC");

        if let Some(limit) = filters.limit {
            param_values.push(limit.min(10_000).to_string());
            sql.push_str(&format!(" LIMIT ?{}", param_values.len()));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(storage_err("failed to prepare event log query"))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_event_log)
            .map_err(storage_err("failed to query event log"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(storage_err("failed to read event log row"))?);
        }
        Ok(results)
    }

    fn prune_event_log(&self, keep_last: u64) -> Result<u64> {
        let conn = self.conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM event_log WHERE rowid <= (
                    SELECT MAX(rowid) - ?1 FROM event_log
                )",
                params![keep_last as i64],
            )
            .map_err(storage_err("failed to prune event log"))?;
        Ok(deleted as u64)
    }
}

fn row_to_event_log(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventLogRecord> {
    let rowid: i64 = row.get("rowid")?;
    let id_str: String = row.get("id")?;
    let id = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let created_at_str: String = row.get("created_at")?;
    let created_at = created_at_str
        .parse::<chrono::DateTime<chrono::Utc>>()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(EventLogRecord::from_stored(
        rowid,
        id,
        row.get("event_type")?,
        row.get("payload")?,
        row.get("summary")?,
        created_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn test_insert_and_list_event_log() {
        let store = test_store();
        let record = EventLogRecord::new(
            uuid::Uuid::new_v4(),
            "file.discovered".into(),
            r#"{"FileDiscovered":{"path":"/test.mkv","size":1024}}"#.into(),
            "path=/test.mkv size=1024".into(),
        );
        let rowid = store.insert_event_log(&record).expect("insert");
        assert!(rowid > 0);

        let records = store
            .list_event_log(&EventLogFilters::default())
            .expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event_type, "file.discovered");
        assert_eq!(records[0].rowid, rowid);
    }

    #[test]
    fn test_list_event_log_with_type_filter() {
        let store = test_store();
        for (i, event_type) in ["file.discovered", "file.introspected", "job.started"]
            .iter()
            .enumerate()
        {
            let record = EventLogRecord::new(
                uuid::Uuid::new_v4(),
                (*event_type).to_string(),
                format!(r#"{{"event":{i}}}"#),
                format!("event {i}"),
            );
            store.insert_event_log(&record).expect("insert");
        }

        // Exact match
        let mut filters = EventLogFilters::default();
        filters.event_type = Some("job.started".into());
        let results = store.list_event_log(&filters).expect("list");
        assert_eq!(results.len(), 1);

        // Wildcard match
        let mut filters = EventLogFilters::default();
        filters.event_type = Some("file.*".into());
        let results = store.list_event_log(&filters).expect("list");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_list_event_log_since_rowid() {
        let store = test_store();
        let mut first_rowid = 0;
        for i in 0..5 {
            let record = EventLogRecord::new(
                uuid::Uuid::new_v4(),
                "file.discovered".into(),
                format!(r#"{{"n":{i}}}"#),
                format!("event {i}"),
            );
            let rowid = store.insert_event_log(&record).expect("insert");
            if i == 0 {
                first_rowid = rowid;
            }
        }

        let mut filters = EventLogFilters::default();
        filters.since_rowid = Some(first_rowid + 2);
        let results = store.list_event_log(&filters).expect("list");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_list_event_log_with_limit() {
        let store = test_store();
        for i in 0..10 {
            let record = EventLogRecord::new(
                uuid::Uuid::new_v4(),
                "file.discovered".into(),
                format!(r#"{{"n":{i}}}"#),
                format!("event {i}"),
            );
            store.insert_event_log(&record).expect("insert");
        }

        let mut filters = EventLogFilters::default();
        filters.limit = Some(3);
        let results = store.list_event_log(&filters).expect("list");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_prune_event_log() {
        let store = test_store();
        for i in 0..20 {
            let record = EventLogRecord::new(
                uuid::Uuid::new_v4(),
                "file.discovered".into(),
                format!(r#"{{"n":{i}}}"#),
                format!("event {i}"),
            );
            store.insert_event_log(&record).expect("insert");
        }

        let pruned = store.prune_event_log(10).expect("prune");
        assert_eq!(pruned, 10);

        let remaining = store
            .list_event_log(&EventLogFilters::default())
            .expect("list");
        assert_eq!(remaining.len(), 10);
    }

    #[test]
    fn test_prune_event_log_empty() {
        let store = test_store();
        let pruned = store.prune_event_log(100).expect("prune");
        assert_eq!(pruned, 0);
    }
}
