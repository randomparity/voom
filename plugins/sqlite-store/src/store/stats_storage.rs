use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::stats::ProcessingStats;
use voom_domain::storage::StatsStorage;

use super::{format_datetime, storage_err, SqliteStore};

impl StatsStorage for SqliteStore {
    fn record_stats(&self, stats: &ProcessingStats) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO processing_stats (id, file_id, policy_name, phase_name, outcome, duration_ms, actions_taken, tracks_modified, file_size_before, file_size_after, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                stats.id.to_string(),
                stats.file_id.to_string(),
                stats.policy_name,
                stats.phase_name,
                stats.outcome.as_str(),
                stats.duration_ms as i64,
                i64::from(stats.actions_taken),
                i64::from(stats.tracks_modified),
                stats.file_size_before.map(|v| v as i64),
                stats.file_size_after.map(|v| v as i64),
                format_datetime(&stats.created_at),
            ],
        )
        .map_err(storage_err("failed to record stats"))?;
        Ok(())
    }
}
