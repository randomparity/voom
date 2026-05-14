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
        // `PluginInvocationOutcome` is `#[non_exhaustive]`: future variants
        // bucket to `unknown` until someone teaches `outcome_to_sql` about
        // them. The retention queries already treat unknown labels the same
        // as any string column, so this is safe.
        _ => ("unknown", None),
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

    fn rollup_plugin_stats(&self, filter: &PluginStatsFilter) -> Result<Vec<PluginStatsRollup>> {
        let conn = self.conn()?;
        let since = filter.since.as_ref().map(iso);
        // Pull rows that match the filter; we compute percentiles in Rust.
        let sql = "SELECT plugin_id, duration_ms, outcome
                   FROM plugin_stats
                   WHERE (?1 IS NULL OR plugin_id = ?1)
                     AND (?2 IS NULL OR started_at >= ?2)
                   ORDER BY plugin_id ASC, duration_ms ASC";

        let mut stmt = conn
            .prepare(sql)
            .map_err(storage_err("failed to prepare rollup query"))?;
        let mut rows = stmt
            .query(params![filter.plugin, since])
            .map_err(storage_err("failed to run rollup query"))?;

        #[derive(Default)]
        struct Bucket {
            durs: Vec<u64>,
            ok: u64,
            skipped: u64,
            err: u64,
            panic: u64,
            total_ms: u64,
        }
        use std::collections::BTreeMap;
        let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
        while let Some(row) = rows
            .next()
            .map_err(storage_err("failed to read rollup row"))?
        {
            let plugin: String = row.get(0).map_err(storage_err("plugin_id col"))?;
            let dur: i64 = row.get(1).map_err(storage_err("duration_ms col"))?;
            let outcome: String = row.get(2).map_err(storage_err("outcome col"))?;
            let dur_u = u64::try_from(dur).unwrap_or(0);
            let entry = buckets.entry(plugin).or_default();
            entry.durs.push(dur_u);
            entry.total_ms = entry.total_ms.saturating_add(dur_u);
            match outcome.as_str() {
                "ok" => entry.ok += 1,
                "skipped" => entry.skipped += 1,
                "err" => entry.err += 1,
                "panic" => entry.panic += 1,
                _ => {}
            }
        }

        use voom_domain::plugin_stats::nearest_rank_percentile;

        let mut out: Vec<PluginStatsRollup> = buckets
            .into_iter()
            .map(|(plugin_id, b)| {
                // b.durs already sorted ASC by SQL ORDER BY.
                // Use `..Default::default()` because `PluginStatsRollup`
                // is `#[non_exhaustive]`: struct-literal construction
                // from outside its defining crate requires it.
                PluginStatsRollup {
                    plugin_id,
                    invocation_count: b.durs.len() as u64,
                    ok_count: b.ok,
                    skipped_count: b.skipped,
                    err_count: b.err,
                    panic_count: b.panic,
                    p50_ms: nearest_rank_percentile(&b.durs, 50),
                    p95_ms: nearest_rank_percentile(&b.durs, 95),
                    p99_ms: nearest_rank_percentile(&b.durs, 99),
                    total_ms: b.total_ms,
                    ..Default::default()
                }
            })
            .collect();

        out.sort_by(|a, b| b.p95_ms.cmp(&a.p95_ms));
        if let Some(top) = filter.top {
            out.truncate(top);
        }
        Ok(out)
    }

    fn prune_old_plugin_stats(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row("SELECT COUNT(*) FROM plugin_stats", [], |row| row.get(0))
                .map_err(storage_err("failed to count plugin_stats"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted = conn
            .execute(
                "WITH ranked AS (
                    SELECT rowid,
                           ROW_NUMBER() OVER (ORDER BY started_at DESC, rowid DESC) AS rn,
                           started_at
                    FROM plugin_stats
                )
                DELETE FROM plugin_stats
                WHERE rowid IN (
                    SELECT rowid FROM ranked
                    WHERE (?1 IS NOT NULL AND started_at < ?1)
                       OR (?2 IS NOT NULL AND rn > ?2)
                )",
                params![cutoff, keep_last],
            )
            .map_err(storage_err("failed to prune plugin_stats"))? as u64;

        let kept: u64 = conn
            .query_row("SELECT COUNT(*) FROM plugin_stats", [], |row| row.get(0))
            .map_err(storage_err("failed to count remaining plugin_stats"))?;
        Ok(PruneReport { deleted, kept })
    }

    fn count_old_plugin_stats(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row("SELECT COUNT(*) FROM plugin_stats", [], |row| row.get(0))
                .map_err(storage_err("failed to count plugin_stats"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted: u64 = conn
            .query_row(
                "WITH ranked AS (
                    SELECT rowid,
                           ROW_NUMBER() OVER (ORDER BY started_at DESC, rowid DESC) AS rn,
                           started_at
                    FROM plugin_stats
                )
                SELECT COUNT(*) FROM ranked
                WHERE (?1 IS NOT NULL AND started_at < ?1)
                   OR (?2 IS NOT NULL AND rn > ?2)",
                params![cutoff, keep_last],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count old plugin_stats"))?;
        let total: u64 = conn
            .query_row("SELECT COUNT(*) FROM plugin_stats", [], |row| row.get(0))
            .map_err(storage_err("failed to count plugin_stats"))?;
        Ok(PruneReport {
            deleted,
            kept: total.saturating_sub(deleted),
        })
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
        PluginStatRecord::new(plugin, "file.discovered", Utc::now(), dur, outcome)
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

    #[test]
    fn rollup_groups_by_plugin_and_computes_percentiles() {
        let s = store();
        // 100 rows for "discovery" with durations 1..=100
        let batch: Vec<_> = (1..=100u64)
            .map(|d| rec("discovery", d, PluginInvocationOutcome::Ok))
            .collect();
        s.insert_plugin_stats_batch(&batch).unwrap();
        // 10 rows for "ffprobe-introspector" with mixed outcomes
        let mut batch2 = Vec::new();
        for _i in 0..8 {
            batch2.push(rec("ffprobe-introspector", 10, PluginInvocationOutcome::Ok));
        }
        batch2.push(rec(
            "ffprobe-introspector",
            20,
            PluginInvocationOutcome::Err {
                category: "io".into(),
            },
        ));
        batch2.push(rec(
            "ffprobe-introspector",
            30,
            PluginInvocationOutcome::Panic,
        ));
        s.insert_plugin_stats_batch(&batch2).unwrap();

        let mut rollup = s
            .rollup_plugin_stats(&PluginStatsFilter::default())
            .unwrap();
        rollup.sort_by(|a, b| a.plugin_id.cmp(&b.plugin_id));
        assert_eq!(rollup.len(), 2);

        let disc = rollup.iter().find(|r| r.plugin_id == "discovery").unwrap();
        assert_eq!(disc.invocation_count, 100);
        assert_eq!(disc.ok_count, 100);
        // Nearest-rank percentile on durations 1..=100
        assert_eq!(disc.p50_ms, 50);
        assert_eq!(disc.p95_ms, 95);
        assert_eq!(disc.p99_ms, 99);

        let ffp = rollup
            .iter()
            .find(|r| r.plugin_id == "ffprobe-introspector")
            .unwrap();
        assert_eq!(ffp.invocation_count, 10);
        assert_eq!(ffp.ok_count, 8);
        assert_eq!(ffp.err_count, 1);
        assert_eq!(ffp.panic_count, 1);
    }

    #[test]
    fn rollup_filter_by_plugin() {
        let s = store();
        s.insert_plugin_stat(&rec("a", 1, PluginInvocationOutcome::Ok))
            .unwrap();
        s.insert_plugin_stat(&rec("b", 2, PluginInvocationOutcome::Ok))
            .unwrap();
        let filter = PluginStatsFilter {
            plugin: Some("a".into()),
            ..Default::default()
        };
        let rollup = s.rollup_plugin_stats(&filter).unwrap();
        assert_eq!(rollup.len(), 1);
        assert_eq!(rollup[0].plugin_id, "a");
    }

    #[test]
    fn rollup_filter_by_since() {
        let s = store();
        let old = PluginStatRecord::new(
            "a",
            "x",
            Utc::now() - chrono::Duration::hours(2),
            1,
            PluginInvocationOutcome::Ok,
        );
        let new = PluginStatRecord::new(
            "a",
            "x",
            Utc::now(),
            2,
            PluginInvocationOutcome::Ok,
        );
        s.insert_plugin_stat(&old).unwrap();
        s.insert_plugin_stat(&new).unwrap();
        let filter =
            PluginStatsFilter::new(None, Some(Utc::now() - chrono::Duration::hours(1)), None);
        let rollup = s.rollup_plugin_stats(&filter).unwrap();
        assert_eq!(rollup[0].invocation_count, 1);
        assert_eq!(rollup[0].p50_ms, 2);
    }

    #[test]
    fn rollup_top_n_sorts_by_p95_descending() {
        let s = store();
        for i in 0..20 {
            s.insert_plugin_stat(&rec("fast", 1, PluginInvocationOutcome::Ok))
                .unwrap();
            s.insert_plugin_stat(&rec("slow", 100 + i, PluginInvocationOutcome::Ok))
                .unwrap();
        }
        let filter = PluginStatsFilter::new(None, None, Some(1));
        let rollup = s.rollup_plugin_stats(&filter).unwrap();
        assert_eq!(rollup.len(), 1);
        assert_eq!(rollup[0].plugin_id, "slow");
    }

    #[test]
    fn prune_with_max_age_deletes_old_rows() {
        let s = store();
        let old = PluginStatRecord::new(
            "a",
            "x",
            Utc::now() - chrono::Duration::days(60),
            1,
            PluginInvocationOutcome::Ok,
        );
        let recent = PluginStatRecord::new(
            "a",
            "x",
            Utc::now(),
            2,
            PluginInvocationOutcome::Ok,
        );
        s.insert_plugin_stat(&old).unwrap();
        s.insert_plugin_stat(&recent).unwrap();

        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(30)),
            keep_last: None,
        };
        let report = s.prune_old_plugin_stats(policy).unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(report.kept, 1);
    }

    #[test]
    fn prune_with_keep_last_deletes_excess_rows() {
        let s = store();
        for i in 0..10 {
            s.insert_plugin_stat(&rec("a", i, PluginInvocationOutcome::Ok))
                .unwrap();
        }
        let policy = RetentionPolicy {
            max_age: None,
            keep_last: Some(3),
        };
        let report = s.prune_old_plugin_stats(policy).unwrap();
        assert_eq!(report.deleted, 7);
        assert_eq!(report.kept, 3);
    }

    #[test]
    fn count_old_matches_actual_prune() {
        let s = store();
        for _ in 0..5 {
            let old = PluginStatRecord::new(
                "a",
                "x",
                Utc::now() - chrono::Duration::days(60),
                1,
                PluginInvocationOutcome::Ok,
            );
            s.insert_plugin_stat(&old).unwrap();
        }
        for _ in 0..3 {
            s.insert_plugin_stat(&rec("a", 1, PluginInvocationOutcome::Ok))
                .unwrap();
        }
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(30)),
            keep_last: None,
        };
        let count = s.count_old_plugin_stats(policy).unwrap();
        assert_eq!(count.deleted, 5);
        assert_eq!(count.kept, 3);
        let prune = s.prune_old_plugin_stats(policy).unwrap();
        assert_eq!(prune.deleted, count.deleted);
        assert_eq!(prune.kept, count.kept);
    }

    #[test]
    fn disabled_policy_is_noop() {
        let s = store();
        s.insert_plugin_stat(&rec("a", 1, PluginInvocationOutcome::Ok))
            .unwrap();
        let policy = RetentionPolicy::default();
        let report = s.prune_old_plugin_stats(policy).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept, 1);
    }
}
