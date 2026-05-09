use std::path::PathBuf;

use rusqlite::params;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::stats::{ProcessingOutcome, SavingsBucket, SavingsReport, TimePeriod};
use voom_domain::storage::{FileTransitionStorage, PruneReport, RetentionPolicy};
use voom_domain::transition::{FileTransition, TransitionSource};

use super::{format_datetime, storage_err, SqliteStore};

/// Insert a `FileTransition` into `file_transitions`. Accepts any
/// `&rusqlite::Connection`, including one obtained via `Deref` from an
/// open `Transaction` — so callers can either run the insert standalone
/// (autocommit) or as part of a larger atomic bundle.
pub(super) fn insert_full_transition_in_tx(
    conn: &rusqlite::Connection,
    t: &FileTransition,
) -> Result<()> {
    conn.execute(
        "INSERT INTO file_transitions \
         (id, file_id, path, from_path, from_hash, to_hash, from_size, to_size, \
          source, source_detail, plan_id, \
          duration_ms, actions_taken, tracks_modified, outcome, \
          policy_name, phase_name, metadata_snapshot, \
          error_message, session_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, \
                 ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
        params![
            t.id.to_string(),
            t.file_id.to_string(),
            t.path.to_string_lossy().to_string(),
            t.from_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            t.from_hash.as_deref().filter(|s| !s.is_empty()),
            t.to_hash,
            t.from_size.map(|v| v as i64),
            t.to_size as i64,
            t.source.as_str(),
            t.source_detail.as_deref().filter(|s| !s.is_empty()),
            t.plan_id.map(|id| id.to_string()),
            t.duration_ms.map(|v| v as i64),
            t.actions_taken.map(i64::from),
            t.tracks_modified.map(i64::from),
            t.outcome.map(|o| o.as_str()),
            t.policy_name.as_deref(),
            t.phase_name.as_deref(),
            t.metadata_snapshot.as_ref().and_then(|s| {
                s.to_json()
                    .map_err(
                        |e| tracing::warn!(error = %e, "failed to serialize metadata_snapshot"),
                    )
                    .ok()
            }),
            t.error_message.as_deref(),
            t.session_id.map(|id| id.to_string()),
            format_datetime(&t.created_at),
        ],
    )
    .map_err(storage_err("failed to insert transition"))?;
    Ok(())
}

impl FileTransitionStorage for SqliteStore {
    fn record_transition(&self, t: &FileTransition) -> Result<()> {
        let conn = self.conn()?;
        insert_full_transition_in_tx(&conn, t)
    }

    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, metadata_snapshot, created_at \
                 FROM file_transitions WHERE file_id = ?1 ORDER BY created_at",
            )
            .map_err(storage_err("failed to prepare transitions_for_file query"))?;

        let rows = stmt
            .query_map(params![file_id.to_string()], row_to_transition)
            .map_err(storage_err("failed to query transitions for file"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect transitions for file"))?;

        Ok(rows)
    }

    fn transitions_by_source(&self, source: TransitionSource) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, metadata_snapshot, created_at \
                 FROM file_transitions WHERE source = ?1 ORDER BY created_at",
            )
            .map_err(storage_err("failed to prepare transitions_by_source query"))?;

        let rows = stmt
            .query_map(params![source.as_str()], row_to_transition)
            .map_err(storage_err("failed to query transitions by source"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect transitions by source"))?;

        Ok(rows)
    }

    fn savings_by_provenance(&self, period: Option<TimePeriod>) -> Result<SavingsReport> {
        let conn = self.conn()?;

        let (period_col, group_by_period) = match period {
            Some(p) => (format!("strftime('{}', created_at)", p.sql_format()), true),
            None => ("NULL".to_string(), false),
        };

        let sql = format!(
            "SELECT source_detail, phase_name, {period_col} AS period, \
                    COUNT(*) AS cnt, \
                    COALESCE(SUM(CASE WHEN from_size IS NOT NULL \
                        THEN from_size - to_size ELSE 0 END), 0) AS saved, \
                    COALESCE(SUM(duration_ms), 0) AS dur, \
                    COUNT(DISTINCT file_id) AS files \
             FROM file_transitions \
             WHERE source = 'voom' AND outcome = 'success' \
             GROUP BY source_detail, phase_name{} \
             ORDER BY saved DESC",
            if group_by_period { ", period" } else { "" },
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(storage_err("failed to prepare savings_by_provenance query"))?;

        let buckets: Vec<SavingsBucket> = stmt
            .query_map([], |row| {
                let executor: Option<String> = row.get("source_detail")?;
                let phase: Option<String> = row.get("phase_name")?;
                let period_val: Option<String> = row.get("period")?;
                let cnt: i64 = row.get("cnt")?;
                let saved: i64 = row.get("saved")?;
                let dur: i64 = row.get("dur")?;
                let files: i64 = row.get("files")?;
                Ok(SavingsBucket::new(
                    executor.filter(|s| !s.is_empty()),
                    phase.filter(|s| !s.is_empty()),
                    period_val.filter(|s| !s.is_empty()),
                    cnt as u64,
                    saved,
                    dur as u64,
                    files as u64,
                ))
            })
            .map_err(storage_err("failed to query savings by provenance"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect savings by provenance"))?;

        let total_bytes_saved: i64 = buckets.iter().map(|b| b.bytes_saved).sum();
        let total_transitions: u64 = buckets.iter().map(|b| b.transition_count).sum();

        Ok(SavingsReport::new(
            buckets,
            total_bytes_saved,
            total_transitions,
        ))
    }

    fn transitions_for_path(&self, path: &std::path::Path) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, metadata_snapshot, created_at \
                 FROM file_transitions \
                 WHERE path = ?1 OR from_path = ?1 \
                 ORDER BY created_at ASC",
            )
            .map_err(storage_err("failed to prepare transitions_for_path query"))?;

        let rows = stmt
            .query_map(
                params![path.to_string_lossy().to_string()],
                row_to_transition,
            )
            .map_err(storage_err("failed to query transitions for path"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect transitions for path"))?;

        Ok(rows)
    }

    fn failed_transitions_for_session(
        &self,
        session_id: &Uuid,
    ) -> Result<Vec<voom_domain::storage::FailedTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT ft.path, ft.phase_name, ft.error_message, \
                 ft.session_id, ft.created_at, p.result \
                 FROM file_transitions ft \
                 LEFT JOIN plans p ON ft.plan_id = p.id \
                 WHERE ft.outcome = 'failure' \
                   AND ft.session_id = ?1 \
                 ORDER BY ft.created_at",
            )
            .map_err(storage_err("failed to prepare failed_transitions query"))?;

        let rows = stmt
            .query_map(params![session_id.to_string()], |row| {
                let path_str: String = row.get("path")?;
                let phase_name: Option<String> = row.get("phase_name")?;
                let error_message: Option<String> = row.get("error_message")?;
                let session_str: Option<String> = row.get("session_id")?;
                let created_at_str: String = row.get("created_at")?;
                let plan_result: Option<String> = row.get("result")?;

                let session_id = session_str
                    .filter(|s| !s.is_empty())
                    .and_then(|s| Uuid::parse_str(&s).ok());
                let created_at = created_at_str.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        format!("corrupt datetime: {e}").into(),
                    )
                })?;

                Ok(voom_domain::storage::FailedTransition {
                    path: PathBuf::from(path_str),
                    phase_name,
                    error_message,
                    session_id,
                    created_at,
                    plan_result,
                })
            })
            .map_err(storage_err("failed to query failed transitions"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect failed transitions"))?;

        Ok(rows)
    }

    fn latest_failure_session(&self) -> Result<Option<Uuid>> {
        let conn = self.conn()?;
        let result: Option<String> = conn
            .query_row(
                "SELECT session_id FROM file_transitions \
                 WHERE outcome = 'failure' AND session_id IS NOT NULL \
                 ORDER BY created_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to query latest failure session"))?;

        Ok(result.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    fn failure_sessions(&self) -> Result<Vec<voom_domain::storage::SessionSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT session_id, MIN(created_at) as started, COUNT(*) as cnt \
                 FROM file_transitions \
                 WHERE outcome = 'failure' AND session_id IS NOT NULL \
                 GROUP BY session_id \
                 ORDER BY MIN(created_at) DESC \
                 LIMIT 20",
            )
            .map_err(storage_err("failed to prepare failure_sessions query"))?;

        let rows = stmt
            .query_map([], |row| {
                let session_str: String = row.get("session_id")?;
                let started_str: String = row.get("started")?;
                let count: i64 = row.get("cnt")?;
                let session_id = Uuid::parse_str(&session_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        format!("invalid UUID: {e}").into(),
                    )
                })?;
                let started_at = started_str.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        format!("corrupt datetime: {e}").into(),
                    )
                })?;
                Ok(voom_domain::storage::SessionSummary {
                    session_id,
                    started_at,
                    failure_count: count as u64,
                })
            })
            .map_err(storage_err("failed to query failure sessions"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect failure sessions"))?;

        Ok(rows)
    }

    fn prune_old_file_transitions(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM file_transitions
                     WHERE rowid NOT IN (SELECT MAX(rowid) FROM file_transitions GROUP BY file_id)",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err("failed to count file_transitions"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted = conn
            .execute(
                "WITH last_per_file AS (
                    SELECT MAX(rowid) AS keep_rowid
                    FROM file_transitions
                    GROUP BY file_id
                ),
                ranked AS (
                    SELECT rowid,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC) AS rn,
                           created_at
                    FROM file_transitions
                    WHERE rowid NOT IN (SELECT keep_rowid FROM last_per_file)
                )
                DELETE FROM file_transitions
                WHERE rowid IN (
                    SELECT rowid FROM ranked
                    WHERE (?1 IS NOT NULL AND created_at < ?1)
                       OR (?2 IS NOT NULL AND rn > ?2)
                )",
                params![cutoff, keep_last],
            )
            .map_err(storage_err("failed to prune file_transitions"))? as u64;

        // `kept` counts only the rows that were *eligible* for pruning (i.e.,
        // not the most-recent-per-file). The pinned most-recent rows are
        // intentionally excluded — `deleted + kept` therefore equals the
        // eligible-set size, not the total table size.
        let kept: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions
                 WHERE rowid NOT IN (SELECT MAX(rowid) FROM file_transitions GROUP BY file_id)",
                [],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count remaining file_transitions"))?;

        Ok(PruneReport { deleted, kept })
    }

    fn count_old_file_transitions(&self, policy: RetentionPolicy) -> Result<PruneReport> {
        if policy.is_disabled() {
            let conn = self.conn()?;
            let kept: u64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM file_transitions
                     WHERE rowid NOT IN (SELECT MAX(rowid) FROM file_transitions GROUP BY file_id)",
                    [],
                    |row| row.get(0),
                )
                .map_err(storage_err("failed to count file_transitions"))?;
            return Ok(PruneReport { deleted: 0, kept });
        }

        let conn = self.conn()?;
        let cutoff = policy.cutoff_str();
        let keep_last = policy.keep_last_i64();

        let deleted: u64 = conn
            .query_row(
                "WITH last_per_file AS (
                    SELECT MAX(rowid) AS keep_rowid
                    FROM file_transitions
                    GROUP BY file_id
                ),
                ranked AS (
                    SELECT rowid,
                           ROW_NUMBER() OVER (ORDER BY created_at DESC) AS rn,
                           created_at
                    FROM file_transitions
                    WHERE rowid NOT IN (SELECT keep_rowid FROM last_per_file)
                )
                SELECT COUNT(*) FROM ranked
                WHERE (?1 IS NOT NULL AND created_at < ?1)
                   OR (?2 IS NOT NULL AND rn > ?2)",
                params![cutoff, keep_last],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count old file_transitions"))?;

        let eligible: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions
                 WHERE rowid NOT IN (SELECT MAX(rowid) FROM file_transitions GROUP BY file_id)",
                [],
                |row| row.get(0),
            )
            .map_err(storage_err("failed to count eligible file_transitions"))?;

        Ok(PruneReport {
            deleted,
            kept: eligible.saturating_sub(deleted),
        })
    }
}

fn row_to_transition(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTransition> {
    let id_str: String = row.get("id")?;
    let file_id_str: String = row.get("file_id")?;
    let path_str: String = row.get("path")?;
    let from_path_str: Option<String> = row.get("from_path")?;
    let from_hash: Option<String> = row.get("from_hash")?;
    let to_hash: String = row.get("to_hash")?;
    let from_size: Option<i64> = row.get("from_size")?;
    let to_size: i64 = row.get("to_size")?;
    let source_str: String = row.get("source")?;
    let source_detail: Option<String> = row.get("source_detail")?;
    let plan_id_str: Option<String> = row.get("plan_id")?;
    let created_at_str: String = row.get("created_at")?;

    let id = parse_uuid_for_row(&id_str, "file_transitions.id")?;
    let file_id = parse_uuid_for_row(&file_id_str, "file_transitions.file_id")?;
    let source = TransitionSource::parse(&source_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown transition source in file_transitions.source: {source_str}").into(),
        )
    })?;
    let plan_id = plan_id_str
        .filter(|s| !s.is_empty())
        .map(|s| parse_uuid_for_row(&s, "file_transitions.plan_id"))
        .transpose()?;
    let created_at = created_at_str.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("corrupt datetime in file_transitions.created_at: {created_at_str}: {e}")
                .into(),
        )
    })?;

    let duration_ms: Option<i64> = row.get("duration_ms")?;
    let actions_taken: Option<i64> = row.get("actions_taken")?;
    let tracks_modified: Option<i64> = row.get("tracks_modified")?;
    let outcome_str: Option<String> = row.get("outcome")?;
    let snapshot_json: Option<String> = row.get("metadata_snapshot")?;

    let mut t = FileTransition::new(
        file_id,
        PathBuf::from(path_str),
        to_hash,
        to_size as u64,
        source,
    );
    t.id = id;
    t.from_path = from_path_str.filter(|s| !s.is_empty()).map(PathBuf::from);
    t.from_hash = from_hash.filter(|s| !s.is_empty());
    t.from_size = from_size.map(|v| v as u64);
    t.source_detail = source_detail.filter(|s| !s.is_empty());
    t.plan_id = plan_id;
    t.duration_ms = duration_ms.map(|v| v as u64);
    t.actions_taken = actions_taken.map(|v| v as u32);
    t.tracks_modified = tracks_modified.map(|v| v as u32);
    t.outcome = outcome_str.and_then(|s| {
        ProcessingOutcome::parse(&s).or_else(|| {
            tracing::warn!(value = %s, "unknown ProcessingOutcome in file_transitions");
            None
        })
    });
    t.policy_name = row.get("policy_name")?;
    t.phase_name = row.get("phase_name")?;
    t.metadata_snapshot = snapshot_json.and_then(|s| {
        voom_domain::snapshot::MetadataSnapshot::from_json(&s)
            .map_err(|e| {
                tracing::warn!(error = %e, "corrupt metadata_snapshot JSON");
            })
            .ok()
    });
    t.created_at = created_at;
    Ok(t)
}

fn parse_uuid_for_row(s: &str, field: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("invalid UUID in {field}: {s}: {e}").into(),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use uuid::Uuid;

    use voom_domain::stats::{ProcessingOutcome, TimePeriod};
    use voom_domain::storage::FileTransitionStorage;
    use voom_domain::transition::{FileTransition, TransitionSource};

    use crate::store::SqliteStore;
    use rusqlite::params;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    #[test]
    fn transitions_reject_unknown_source() {
        let store = test_store();
        let file_id = Uuid::new_v4();
        let transition = FileTransition::new(
            file_id,
            PathBuf::from("/media/source.mkv"),
            "hash".into(),
            10,
            TransitionSource::Voom,
        );
        store.record_transition(&transition).unwrap();

        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE file_transitions SET source = ?1 WHERE id = ?2",
            params!["mystery", transition.id.to_string()],
        )
        .unwrap();

        let err = store.transitions_for_file(&file_id).unwrap_err();
        assert!(err.to_string().contains("unknown transition source"));
    }

    #[test]
    fn savings_by_provenance_empty_db() {
        let store = test_store();
        let report = store.savings_by_provenance(None).unwrap();
        assert!(report.buckets.is_empty());
        assert_eq!(report.total_bytes_saved, 0);
        assert_eq!(report.total_transitions, 0);
    }

    #[test]
    fn savings_by_provenance_groups_by_executor_and_phase() {
        let store = test_store();

        let file_a = Uuid::new_v4();
        let file_b = Uuid::new_v4();

        // Two successful voom transitions with different executor/phase combos
        let t1 = FileTransition::new(
            file_a,
            PathBuf::from("/media/a.mkv"),
            "hash_a_new".into(),
            800_000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash_a_old".into()), Some(1_000_000))
        .with_detail("mkvtoolnix")
        .with_processing(
            500,
            2,
            1,
            ProcessingOutcome::Success,
            "default",
            "normalize",
        );

        let t2 = FileTransition::new(
            file_b,
            PathBuf::from("/media/b.mkv"),
            "hash_b_new".into(),
            600_000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash_b_old".into()), Some(900_000))
        .with_detail("ffmpeg")
        .with_processing(
            300,
            1,
            0,
            ProcessingOutcome::Success,
            "default",
            "transcode",
        );

        // A failed transition — should be excluded from savings
        let file_c = Uuid::new_v4();
        let t3 = FileTransition::new(
            file_c,
            PathBuf::from("/media/c.mkv"),
            "hash_c_new".into(),
            500_000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash_c_old".into()), Some(700_000))
        .with_detail("mkvtoolnix")
        .with_processing(
            100,
            0,
            0,
            ProcessingOutcome::Failure,
            "default",
            "normalize",
        );

        store.record_transition(&t1).unwrap();
        store.record_transition(&t2).unwrap();
        store.record_transition(&t3).unwrap();

        let report = store.savings_by_provenance(None).unwrap();

        // Only 2 buckets: the failed transition is excluded
        assert_eq!(report.buckets.len(), 2);
        assert_eq!(report.total_transitions, 2);

        // mkvtoolnix/normalize saved 200_000 bytes
        let mkv_bucket = report
            .buckets
            .iter()
            .find(|b| b.executor.as_deref() == Some("mkvtoolnix"))
            .expect("mkvtoolnix bucket missing");
        assert_eq!(mkv_bucket.bytes_saved, 200_000);
        assert_eq!(mkv_bucket.transition_count, 1);
        assert_eq!(mkv_bucket.phase.as_deref(), Some("normalize"));

        // ffmpeg/transcode saved 300_000 bytes
        let ff_bucket = report
            .buckets
            .iter()
            .find(|b| b.executor.as_deref() == Some("ffmpeg"))
            .expect("ffmpeg bucket missing");
        assert_eq!(ff_bucket.bytes_saved, 300_000);
        assert_eq!(ff_bucket.transition_count, 1);
        assert_eq!(ff_bucket.phase.as_deref(), Some("transcode"));

        assert_eq!(report.total_bytes_saved, 500_000);
    }

    #[test]
    fn savings_by_provenance_with_time_period() {
        let store = test_store();

        let file_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/media/d.mkv"),
            "hash_d_new".into(),
            700_000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash_d_old".into()), Some(1_000_000))
        .with_detail("mkvtoolnix")
        .with_processing(400, 1, 1, ProcessingOutcome::Success, "default", "cleanup");

        store.record_transition(&t).unwrap();

        let report = store
            .savings_by_provenance(Some(TimePeriod::Month))
            .unwrap();

        assert_eq!(report.buckets.len(), 1);
        let bucket = &report.buckets[0];

        // Period should be populated in YYYY-MM format
        let period = bucket.period.as_deref().expect("period should be set");
        assert!(
            period.len() == 7 && period.chars().nth(4) == Some('-'),
            "expected YYYY-MM format, got: {period}"
        );

        assert_eq!(bucket.bytes_saved, 300_000);
        assert_eq!(bucket.transition_count, 1);
    }

    #[test]
    fn transitions_for_path_matches_either_path_column() {
        let store = test_store();
        let file_id = Uuid::new_v4();

        let t = FileTransition::new(
            file_id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".into(),
            900_000,
            TransitionSource::Voom,
        )
        .with_from(Some("old_hash".into()), Some(1_000_000))
        .with_from_path(PathBuf::from("/media/movie.mp4"));
        store.record_transition(&t).unwrap();

        let by_new = store
            .transitions_for_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap();
        assert_eq!(by_new.len(), 1);
        assert_eq!(by_new[0].id, t.id);

        let by_old = store
            .transitions_for_path(std::path::Path::new("/media/movie.mp4"))
            .unwrap();
        assert_eq!(by_old.len(), 1);
        assert_eq!(by_old[0].id, t.id);
        assert_eq!(
            by_old[0].from_path.as_deref(),
            Some(std::path::Path::new("/media/movie.mp4"))
        );
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;
    use voom_domain::storage::RetentionPolicy;
    use voom_domain::transition::{FileTransition, TransitionSource};

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    fn make_transition(file_id: uuid::Uuid, when: chrono::DateTime<chrono::Utc>) -> FileTransition {
        let mut t = FileTransition::new(
            file_id,
            std::path::PathBuf::from("/test.mkv"),
            "h2".into(),
            1024,
            TransitionSource::Discovery,
        );
        t.created_at = when;
        t
    }

    #[test]
    fn prune_old_file_transitions_preserves_latest_per_file() {
        let store = test_store();
        let file_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        // Three transitions for same file: oldest 100d, then 50d, then 1d
        for days in [100i64, 50, 1] {
            let t = make_transition(file_id, now - chrono::Duration::days(days));
            store.record_transition(&t).unwrap();
        }
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(30)),
            keep_last: None,
        };
        let report = store.prune_old_file_transitions(policy).unwrap();
        // Most recent (1d) is preserved; the other two (100d, 50d) are eligible
        // and both exceed the 30d cutoff, so both deleted.
        assert_eq!(report.deleted, 2);
        let remaining = store.transitions_for_file(&file_id).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn prune_old_file_transitions_protects_singleton_old_row() {
        let store = test_store();
        let file_id = uuid::Uuid::new_v4();
        let t = make_transition(file_id, chrono::Utc::now() - chrono::Duration::days(365));
        store.record_transition(&t).unwrap();
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(1)),
            keep_last: None,
        };
        let report = store.prune_old_file_transitions(policy).unwrap();
        assert_eq!(report.deleted, 0, "the only row for a file is always kept");
        assert_eq!(store.transitions_for_file(&file_id).unwrap().len(), 1);
    }

    #[test]
    fn prune_old_file_transitions_disabled_is_noop() {
        let store = test_store();
        let file_id = uuid::Uuid::new_v4();
        for _ in 0..3 {
            store
                .record_transition(&make_transition(file_id, chrono::Utc::now()))
                .unwrap();
        }
        let report = store
            .prune_old_file_transitions(RetentionPolicy::default())
            .unwrap();
        assert_eq!(report.deleted, 0);
        // kept counts the rows the policy was eligible to consider — i.e., everything
        // except the most-recent-per-file. Two are eligible, both kept.
        assert_eq!(report.kept, 2);
    }

    #[test]
    fn prune_old_file_transitions_count_caps_eligible_set() {
        let store = test_store();
        // 5 separate files, one transition each — all are "most recent per file"
        // and therefore exempt.
        for _ in 0..5 {
            store
                .record_transition(&make_transition(uuid::Uuid::new_v4(), chrono::Utc::now()))
                .unwrap();
        }
        let policy = RetentionPolicy {
            max_age: None,
            keep_last: Some(0),
        };
        let report = store.prune_old_file_transitions(policy).unwrap();
        assert_eq!(report.deleted, 0, "all rows are most-recent-per-file");
    }

    #[test]
    fn prune_old_file_transitions_empty_is_noop() {
        let store = test_store();
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(1)),
            keep_last: Some(10),
        };
        let report = store.prune_old_file_transitions(policy).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept, 0);
    }

    #[test]
    fn count_old_file_transitions_matches_prune_count() {
        let store = test_store();
        let now = chrono::Utc::now();
        // Single file with 5 transitions at different ages
        let file_id = uuid::Uuid::new_v4();
        for days in [10i64, 5, 3, 1, 0] {
            let t = make_transition(file_id, now - chrono::Duration::days(days));
            store.record_transition(&t).unwrap();
        }
        let policy = RetentionPolicy {
            max_age: Some(chrono::Duration::days(4)),
            keep_last: Some(3),
        };
        let count_report = store.count_old_file_transitions(policy).unwrap();
        // Verify non-destructive: all 5 rows still present
        let total_before: u64 = store
            .conn()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM file_transitions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            total_before, 5,
            "count_old_file_transitions must not modify the database"
        );
        let prune_report = store.prune_old_file_transitions(policy).unwrap();
        assert_eq!(count_report.deleted, prune_report.deleted);
        assert_eq!(count_report.kept, prune_report.kept);
    }
}
