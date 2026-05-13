//! SQLite-backed `PluginStatsStorage` implementation (issue #92).

use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::plugin_stats::{
    PluginInvocationOutcome, PluginStatRecord, PluginStatsFilter, PluginStatsRollup,
};
use voom_domain::storage::{PluginStatsStorage, PruneReport, RetentionPolicy};

use super::{SqliteStore, storage_err};

fn outcome_to_sql(outcome: &PluginInvocationOutcome) -> (&'static str, Option<&str>) {
    match outcome {
        PluginInvocationOutcome::Ok => ("ok", None),
        PluginInvocationOutcome::Skipped => ("skipped", None),
        PluginInvocationOutcome::Err { category } => ("err", Some(category.as_str())),
        PluginInvocationOutcome::Panic => ("panic", None),
    }
}

fn iso(ts: &chrono::DateTime<chrono::Utc>) -> String {
    voom_domain::utils::format::format_iso(ts)
}

fn insert_one(conn: &rusqlite::Connection, record: &PluginStatRecord) -> Result<()> {
    let (label, category) = outcome_to_sql(&record.outcome);
    conn.execute(
        "INSERT INTO plugin_stats
            (plugin_id, event_type, started_at, duration_ms, outcome, error_category)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            record.plugin_id,
            record.event_type,
            iso(&record.started_at),
            i64::try_from(record.duration_ms).unwrap_or(i64::MAX),
            label,
            category,
        ],
    )
    .map_err(storage_err("failed to insert plugin_stats row"))?;
    Ok(())
}

impl PluginStatsStorage for SqliteStore {
    fn insert_plugin_stat(&self, record: &PluginStatRecord) -> Result<()> {
        let conn = self.conn()?;
        insert_one(&conn, record)
    }

    fn insert_plugin_stats_batch(&self, records: &[PluginStatRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage_err("failed to begin plugin_stats tx"))?;
        for r in records {
            insert_one(&tx, r)?;
        }
        tx.commit()
            .map_err(storage_err("failed to commit plugin_stats tx"))?;
        Ok(())
    }

    fn rollup_plugin_stats(&self, _filter: &PluginStatsFilter) -> Result<Vec<PluginStatsRollup>> {
        // Implemented in Task 5.
        Ok(Vec::new())
    }

    fn prune_old_plugin_stats(&self, _policy: RetentionPolicy) -> Result<PruneReport> {
        // Implemented in Task 6.
        Ok(PruneReport::default())
    }

    fn count_old_plugin_stats(&self, _policy: RetentionPolicy) -> Result<PruneReport> {
        // Implemented in Task 6.
        Ok(PruneReport::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn store() -> SqliteStore {
        SqliteStore::in_memory().expect("store")
    }

    fn rec(plugin: &str, dur: u64, outcome: PluginInvocationOutcome) -> PluginStatRecord {
        PluginStatRecord {
            plugin_id: plugin.into(),
            event_type: "file.discovered".into(),
            started_at: Utc::now(),
            duration_ms: dur,
            outcome,
        }
    }

    #[test]
    fn insert_one_round_trips() {
        let s = store();
        let r = rec("discovery", 12, PluginInvocationOutcome::Ok);
        s.insert_plugin_stat(&r).unwrap();
        let conn = s.conn().unwrap();
        let (plugin, dur, label, cat): (String, i64, String, Option<String>) = conn
            .query_row(
                "SELECT plugin_id, duration_ms, outcome, error_category FROM plugin_stats",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(plugin, "discovery");
        assert_eq!(dur, 12);
        assert_eq!(label, "ok");
        assert!(cat.is_none());
    }

    #[test]
    fn err_outcome_persists_category() {
        let s = store();
        let r = rec(
            "ffmpeg-executor",
            500,
            PluginInvocationOutcome::Err {
                category: "spawn".into(),
            },
        );
        s.insert_plugin_stat(&r).unwrap();
        let conn = s.conn().unwrap();
        let (label, cat): (String, Option<String>) = conn
            .query_row(
                "SELECT outcome, error_category FROM plugin_stats",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(label, "err");
        assert_eq!(cat, Some("spawn".into()));
    }

    #[test]
    fn batch_insert_uses_single_tx() {
        let s = store();
        let records: Vec<_> = (0..100)
            .map(|i| rec("discovery", i, PluginInvocationOutcome::Ok))
            .collect();
        s.insert_plugin_stats_batch(&records).unwrap();
        let conn = s.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 100);
    }
}
