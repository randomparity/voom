use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::{HealthCheckFilters, HealthCheckRecord, HealthCheckStorage};

use super::{format_datetime, storage_err, SqliteStore};

impl HealthCheckStorage for SqliteStore {
    fn insert_health_check(&self, record: &HealthCheckRecord) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO health_checks (id, check_name, passed, details, checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                record.id.to_string(),
                record.check_name,
                record.passed as i32,
                record.details,
                format_datetime(&record.checked_at),
            ],
        )
        .map_err(storage_err("failed to insert health check"))?;
        Ok(())
    }

    fn list_health_checks(&self, filters: &HealthCheckFilters) -> Result<Vec<HealthCheckRecord>> {
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT id, check_name, passed, details, checked_at
             FROM health_checks WHERE 1=1",
        );
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref name) = filters.check_name {
            param_values.push(name.clone());
            sql.push_str(&format!(" AND check_name = ?{}", param_values.len()));
        }
        if let Some(passed) = filters.passed {
            param_values.push((passed as i32).to_string());
            sql.push_str(&format!(" AND passed = ?{}", param_values.len()));
        }
        if let Some(ref since) = filters.since {
            param_values.push(format_datetime(since));
            sql.push_str(&format!(" AND checked_at >= ?{}", param_values.len()));
        }

        sql.push_str(" ORDER BY checked_at DESC");

        if let Some(limit) = filters.limit {
            param_values.push(limit.min(10_000).to_string());
            sql.push_str(&format!(" LIMIT ?{}", param_values.len()));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(storage_err("failed to prepare health check query"))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_health_check)
            .map_err(storage_err("failed to query health checks"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(storage_err("failed to read health check row"))?);
        }
        Ok(results)
    }

    fn latest_health_checks(&self) -> Result<Vec<HealthCheckRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT h.id, h.check_name, h.passed, h.details, h.checked_at
                 FROM health_checks h
                 INNER JOIN (
                     SELECT check_name, MAX(checked_at) AS max_at
                     FROM health_checks
                     GROUP BY check_name
                 ) latest ON h.check_name = latest.check_name
                     AND h.checked_at = latest.max_at
                 ORDER BY h.check_name",
            )
            .map_err(storage_err("failed to prepare latest health checks query"))?;

        let rows = stmt
            .query_map([], row_to_health_check)
            .map_err(storage_err("failed to query latest health checks"))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(storage_err("failed to read health check row"))?);
        }
        Ok(results)
    }

    fn prune_health_checks(&self, before: chrono::DateTime<chrono::Utc>) -> Result<u64> {
        let conn = self.conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM health_checks WHERE checked_at < ?1",
                params![format_datetime(&before)],
            )
            .map_err(storage_err("failed to prune health checks"))?;
        Ok(deleted as u64)
    }
}

fn row_to_health_check(row: &rusqlite::Row<'_>) -> rusqlite::Result<HealthCheckRecord> {
    let id_str: String = row.get("id")?;
    let id = uuid::Uuid::parse_str(&id_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let checked_at_str: String = row.get("checked_at")?;
    let checked_at = checked_at_str
        .parse::<chrono::DateTime<chrono::Utc>>()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?;

    Ok(HealthCheckRecord::from_stored(
        id,
        row.get("check_name")?,
        row.get::<_, i32>("passed")? != 0,
        row.get("details")?,
        checked_at,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    #[test]
    fn test_insert_and_list_health_checks() {
        let store = test_store();
        let record = HealthCheckRecord::new("data_dir_exists", true, None);
        store.insert_health_check(&record).expect("insert");

        let checks = store
            .list_health_checks(&HealthCheckFilters::default())
            .expect("list");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].check_name, "data_dir_exists");
        assert!(checks[0].passed);
    }

    #[test]
    fn test_latest_health_checks() {
        let store = test_store();

        // Insert two records for the same check
        let r1 = HealthCheckRecord::from_stored(
            uuid::Uuid::new_v4(),
            "data_dir_exists".into(),
            false,
            Some("not found".into()),
            Utc::now() - chrono::Duration::hours(1),
        );
        store.insert_health_check(&r1).expect("insert r1");

        let r2 = HealthCheckRecord::new("data_dir_exists", true, Some("ok".into()));
        store.insert_health_check(&r2).expect("insert r2");

        let latest = store.latest_health_checks().expect("latest");
        assert_eq!(latest.len(), 1);
        assert!(latest[0].passed);
        assert_eq!(latest[0].details.as_deref(), Some("ok"));
    }

    #[test]
    fn test_prune_health_checks() {
        let store = test_store();

        let old = HealthCheckRecord::from_stored(
            uuid::Uuid::new_v4(),
            "old_check".into(),
            true,
            None,
            Utc::now() - chrono::Duration::days(60),
        );
        store.insert_health_check(&old).expect("insert old");

        let recent = HealthCheckRecord::new("recent_check", true, None);
        store.insert_health_check(&recent).expect("insert recent");

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let pruned = store.prune_health_checks(cutoff).expect("prune");
        assert_eq!(pruned, 1);

        let remaining = store
            .list_health_checks(&HealthCheckFilters::default())
            .expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].check_name, "recent_check");
    }

    #[test]
    fn test_list_health_checks_with_filters() {
        let store = test_store();

        let r1 = HealthCheckRecord::new("check_a", true, None);
        store.insert_health_check(&r1).expect("insert");

        let r2 = HealthCheckRecord::new("check_b", false, Some("fail".into()));
        store.insert_health_check(&r2).expect("insert");

        // Filter by check_name
        let mut filters = HealthCheckFilters::default();
        filters.check_name = Some("check_a".into());
        let results = store.list_health_checks(&filters).expect("list");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].check_name, "check_a");

        // Filter by passed
        let mut filters = HealthCheckFilters::default();
        filters.passed = Some(false);
        let results = store.list_health_checks(&filters).expect("list");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].check_name, "check_b");

        // Filter with limit
        let mut filters = HealthCheckFilters::default();
        filters.limit = Some(1);
        let results = store.list_health_checks(&filters).expect("list");
        assert_eq!(results.len(), 1);
    }
}
