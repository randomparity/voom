//! `VerificationStorage` implementation backed by SQLite.

use chrono::{DateTime, Utc};
use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::VerificationStorage;
use voom_domain::verification::{
    IntegritySummary, VerificationFilters, VerificationMode, VerificationRecord,
};

use super::row_mappers::row_to_verification;
use super::{format_datetime, storage_err, SqlQuery, SqliteStore};

const SELECT_VERIFICATION: &str = "SELECT id, file_id, verified_at, mode, outcome, \
    error_count, warning_count, content_hash, details FROM verifications";
const INTEGRITY_FILE_AGGREGATES_SQL: &str = "WITH latest AS ( \
        SELECT file_id, MAX(verified_at) AS last_at \
        FROM verifications \
        GROUP BY file_id \
    ) \
    SELECT \
        COUNT(*) AS total_files, \
        COALESCE(SUM(CASE WHEN l.last_at IS NULL THEN 1 ELSE 0 END), 0) AS never_verified, \
        COALESCE(SUM(CASE WHEN l.last_at < ?1 THEN 1 ELSE 0 END), 0) AS stale \
    FROM files f \
    LEFT JOIN latest l ON l.file_id = f.id \
    WHERE f.status = 'active'";
const INTEGRITY_OUTCOME_AGGREGATES_SQL: &str = "WITH latest AS ( \
        SELECT v.file_id, v.outcome, \
            ROW_NUMBER() OVER ( \
                PARTITION BY v.file_id ORDER BY v.verified_at DESC, v.id DESC \
            ) AS rn \
        FROM verifications v \
        JOIN files f ON f.id = v.file_id \
        WHERE f.status = 'active' \
    ) \
    SELECT \
        COALESCE(SUM(CASE WHEN outcome = 'error' THEN 1 ELSE 0 END), 0) AS with_errors, \
        COALESCE(SUM(CASE WHEN outcome = 'warning' THEN 1 ELSE 0 END), 0) AS with_warnings \
    FROM latest \
    WHERE rn = 1";
const INTEGRITY_HASH_MISMATCHES_SQL: &str = "SELECT COUNT(DISTINCT file_id) FROM ( \
        SELECT file_id, content_hash, \
            LAG(content_hash) OVER ( \
                PARTITION BY file_id ORDER BY verified_at \
            ) AS prev_hash, \
            ROW_NUMBER() OVER ( \
                PARTITION BY file_id ORDER BY verified_at DESC \
            ) AS rn \
        FROM verifications WHERE mode = 'hash' \
    ) WHERE rn = 1 AND prev_hash IS NOT NULL AND prev_hash <> content_hash";

impl VerificationStorage for SqliteStore {
    fn insert_verification(&self, record: &VerificationRecord) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO verifications \
             (id, file_id, verified_at, mode, outcome, error_count, warning_count, \
              content_hash, details) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.id.to_string(),
                record.file_id,
                format_datetime(&record.verified_at),
                record.mode.as_str(),
                record.outcome.as_str(),
                i64::from(record.error_count),
                i64::from(record.warning_count),
                record.content_hash,
                record.details,
            ],
        )
        .map_err(storage_err("failed to insert verification"))?;
        Ok(())
    }

    fn list_verifications(&self, filters: &VerificationFilters) -> Result<Vec<VerificationRecord>> {
        let conn = self.conn()?;
        let mut q = SqlQuery::new(&format!("{SELECT_VERIFICATION} WHERE 1=1"));

        if let Some(file_id) = filters.file_id.as_ref() {
            q.condition(" AND file_id = {}", file_id.clone());
        }
        if let Some(mode) = filters.mode {
            q.condition(" AND mode = {}", mode.as_str().to_string());
        }
        if let Some(outcome) = filters.outcome {
            q.condition(" AND outcome = {}", outcome.as_str().to_string());
        }
        if let Some(since) = filters.since.as_ref() {
            q.condition(" AND verified_at >= {}", format_datetime(since));
        }
        q.sql.push_str(" ORDER BY verified_at DESC");
        q.paginate(filters.limit, None);

        let mut stmt = conn
            .prepare(&q.sql)
            .map_err(storage_err("failed to prepare verification query"))?;
        let rows = stmt
            .query_map(q.param_refs().as_slice(), row_to_verification)
            .map_err(storage_err("failed to query verifications"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(storage_err("failed to read verification row"))?);
        }
        Ok(out)
    }

    fn latest_verification(
        &self,
        file_id: &str,
        mode: VerificationMode,
    ) -> Result<Option<VerificationRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "{SELECT_VERIFICATION} WHERE file_id = ?1 AND mode = ?2 \
                 ORDER BY verified_at DESC LIMIT 1"
            ))
            .map_err(storage_err("failed to prepare latest verification query"))?;
        let mut rows = stmt
            .query(params![file_id, mode.as_str()])
            .map_err(storage_err("failed to execute latest verification query"))?;
        match rows
            .next()
            .map_err(storage_err("failed to read latest verification row"))?
        {
            Some(row) => Ok(Some(
                row_to_verification(row)
                    .map_err(storage_err("failed to map latest verification row"))?,
            )),
            None => Ok(None),
        }
    }

    fn integrity_summary(&self, since: DateTime<Utc>) -> Result<IntegritySummary> {
        let conn = self.conn()?;

        let (total_files, never_verified, stale): (i64, i64, i64) = conn
            .query_row(
                INTEGRITY_FILE_AGGREGATES_SQL,
                params![format_datetime(&since)],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(storage_err("failed to compute file integrity aggregates"))?;

        let (with_errors, with_warnings): (i64, i64) = conn
            .query_row(INTEGRITY_OUTCOME_AGGREGATES_SQL, [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(storage_err(
                "failed to compute outcome integrity aggregates",
            ))?;

        let hash_mismatches: i64 = conn
            .query_row(INTEGRITY_HASH_MISMATCHES_SQL, [], |r| r.get(0))
            .map_err(storage_err("failed to count hash mismatches"))?;

        Ok(IntegritySummary::new(
            u64::try_from(total_files.max(0)).unwrap_or(0),
            u64::try_from(never_verified.max(0)).unwrap_or(0),
            u64::try_from(stale.max(0)).unwrap_or(0),
            u64::try_from(with_errors.max(0)).unwrap_or(0),
            u64::try_from(with_warnings.max(0)).unwrap_or(0),
            u64::try_from(hash_mismatches.max(0)).unwrap_or(0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;
    use uuid::Uuid;
    use voom_domain::media::{Container, MediaFile};
    use voom_domain::storage::FileStorage;
    use voom_domain::test_support::InMemoryStore;
    use voom_domain::transition::FileStatus;
    use voom_domain::verification::VerificationOutcome;

    fn store_with_file() -> (SqliteStore, String) {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let file = make_file("/media/test.mkv", FileStatus::Active);
        let file_id = file.id.to_string();
        store.upsert_file(&file).expect("insert file");
        (store, file_id)
    }

    fn make_file(path: &str, status: FileStatus) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path));
        file.size = 1;
        file.content_hash = Some("hash-stub".into());
        file.container = Container::Mkv;
        file.introspected_at = Utc::now();
        file.status = status;
        file
    }

    fn insert_file(store: &SqliteStore, path: &str, status: FileStatus) -> String {
        let file = make_file(path, status);
        let id = file.id.to_string();
        store.upsert_file(&file).expect("insert file");
        id
    }

    fn verification(
        file_id: &str,
        verified_at: DateTime<Utc>,
        mode: VerificationMode,
        outcome: VerificationOutcome,
    ) -> VerificationRecord {
        VerificationRecord::new(
            Uuid::new_v4(),
            file_id.to_string(),
            verified_at,
            mode,
            outcome,
            u32::from(outcome == VerificationOutcome::Error),
            u32::from(outcome == VerificationOutcome::Warning),
            None,
            None,
        )
    }

    #[test]
    fn insert_and_list_verification() {
        let (store, file_id) = store_with_file();
        let record = VerificationRecord::new(
            Uuid::new_v4(),
            file_id.clone(),
            Utc::now(),
            VerificationMode::Quick,
            VerificationOutcome::Ok,
            0,
            0,
            None,
            None,
        );
        store.insert_verification(&record).expect("insert");

        let mut filters = VerificationFilters::default();
        filters.file_id = Some(file_id);
        let listed = store.list_verifications(&filters).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, record.id);
        assert_eq!(listed[0].mode, VerificationMode::Quick);
        assert_eq!(listed[0].outcome, VerificationOutcome::Ok);
    }

    #[test]
    fn latest_verification_returns_newest() {
        let (store, file_id) = store_with_file();
        let earlier = VerificationRecord::new(
            Uuid::new_v4(),
            file_id.clone(),
            Utc::now() - chrono::Duration::hours(1),
            VerificationMode::Hash,
            VerificationOutcome::Ok,
            0,
            0,
            Some("hash-a".into()),
            None,
        );
        let later = VerificationRecord::new(
            Uuid::new_v4(),
            file_id.clone(),
            Utc::now(),
            VerificationMode::Hash,
            VerificationOutcome::Ok,
            0,
            0,
            Some("hash-b".into()),
            None,
        );
        store.insert_verification(&earlier).expect("insert earlier");
        store.insert_verification(&later).expect("insert later");

        let got = store
            .latest_verification(&file_id, VerificationMode::Hash)
            .expect("latest")
            .expect("some");
        assert_eq!(got.id, later.id);
        assert_eq!(got.content_hash.as_deref(), Some("hash-b"));
    }

    #[test]
    fn integrity_summary_counts_never_verified() {
        let (store, _file_id) = store_with_file();
        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .expect("summary");
        assert_eq!(summary.total_files, 1);
        assert_eq!(summary.never_verified, 1);
        assert_eq!(summary.stale, 0);
        assert_eq!(summary.with_errors, 0);
        assert_eq!(summary.with_warnings, 0);
        assert_eq!(summary.hash_mismatches, 0);
    }

    #[test]
    fn integrity_summary_empty_store_is_zeroed() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .expect("summary");
        assert_eq!(summary, IntegritySummary::new(0, 0, 0, 0, 0, 0));
    }

    #[test]
    fn integrity_summary_counts_mixed_active_states() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let cutoff = Utc::now() - chrono::Duration::days(7);
        let stale_time = cutoff - chrono::Duration::seconds(1);
        let fresh_time = cutoff + chrono::Duration::seconds(1);

        insert_file(&store, "/media/never.mkv", FileStatus::Active);
        let stale_id = insert_file(&store, "/media/stale.mkv", FileStatus::Active);
        let error_id = insert_file(&store, "/media/error.mkv", FileStatus::Active);
        let warning_id = insert_file(&store, "/media/warning.mkv", FileStatus::Active);
        let missing_id = insert_file(&store, "/media/missing.mkv", FileStatus::Missing);

        for record in [
            verification(
                &stale_id,
                stale_time,
                VerificationMode::Quick,
                VerificationOutcome::Ok,
            ),
            verification(
                &error_id,
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Error,
            ),
            verification(
                &warning_id,
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Warning,
            ),
            verification(
                &missing_id,
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Error,
            ),
        ] {
            store
                .insert_verification(&record)
                .expect("insert verification");
        }

        let summary = store.integrity_summary(cutoff).expect("summary");
        assert_eq!(summary, IntegritySummary::new(4, 1, 1, 1, 1, 0));
    }

    #[test]
    fn integrity_summary_latest_outcome_tie_uses_highest_id() {
        let (store, file_id) = store_with_file();
        let verified_at = Utc::now();
        let low_id = Uuid::from_u128(1);
        let high_id = Uuid::from_u128(2);

        let older_by_id = VerificationRecord::new(
            low_id,
            file_id.clone(),
            verified_at,
            VerificationMode::Quick,
            VerificationOutcome::Error,
            1,
            0,
            None,
            None,
        );
        let latest_by_id = VerificationRecord::new(
            high_id,
            file_id,
            verified_at,
            VerificationMode::Quick,
            VerificationOutcome::Warning,
            0,
            1,
            None,
            None,
        );
        store
            .insert_verification(&older_by_id)
            .expect("insert older");
        store
            .insert_verification(&latest_by_id)
            .expect("insert latest");

        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .expect("summary");
        assert_eq!(summary.with_errors, 0);
        assert_eq!(summary.with_warnings, 1);
    }

    #[test]
    fn integrity_summary_matches_in_memory_store_for_mixed_fixture() {
        let sqlite = SqliteStore::in_memory().expect("in-memory sqlite");
        let memory = InMemoryStore::new();
        let cutoff = Utc::now() - chrono::Duration::days(7);

        let files = [
            make_file("/media/never.mkv", FileStatus::Active),
            make_file("/media/stale.mkv", FileStatus::Active),
            make_file("/media/error.mkv", FileStatus::Active),
            make_file("/media/warning.mkv", FileStatus::Active),
            make_file("/media/missing.mkv", FileStatus::Missing),
        ];
        for file in &files {
            sqlite.upsert_file(file).expect("sqlite insert file");
            memory.upsert_file(file).expect("memory insert file");
        }

        let stale_time = cutoff - chrono::Duration::seconds(1);
        let fresh_time = cutoff + chrono::Duration::seconds(1);
        let records = [
            verification(
                &files[1].id.to_string(),
                stale_time,
                VerificationMode::Quick,
                VerificationOutcome::Ok,
            ),
            verification(
                &files[2].id.to_string(),
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Error,
            ),
            verification(
                &files[3].id.to_string(),
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Warning,
            ),
            verification(
                &files[4].id.to_string(),
                fresh_time,
                VerificationMode::Quick,
                VerificationOutcome::Error,
            ),
        ];
        for record in &records {
            sqlite
                .insert_verification(record)
                .expect("sqlite insert verification");
            memory
                .insert_verification(record)
                .expect("memory insert verification");
        }

        assert_eq!(
            sqlite.integrity_summary(cutoff).expect("sqlite summary"),
            memory.integrity_summary(cutoff).expect("memory summary")
        );
    }

    #[test]
    fn file_aggregate_query_returns_total_never_and_stale_counts() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let cutoff = Utc::now() - chrono::Duration::days(7);
        insert_file(&store, "/media/never.mkv", FileStatus::Active);
        let stale_id = insert_file(&store, "/media/stale.mkv", FileStatus::Active);
        let fresh_id = insert_file(&store, "/media/fresh.mkv", FileStatus::Active);

        for record in [
            verification(
                &stale_id,
                cutoff - chrono::Duration::seconds(1),
                VerificationMode::Quick,
                VerificationOutcome::Ok,
            ),
            verification(
                &fresh_id,
                cutoff + chrono::Duration::seconds(1),
                VerificationMode::Quick,
                VerificationOutcome::Ok,
            ),
        ] {
            store
                .insert_verification(&record)
                .expect("insert verification");
        }

        let counts: (i64, i64, i64) = store
            .conn()
            .expect("connection")
            .query_row(
                INTEGRITY_FILE_AGGREGATES_SQL,
                params![format_datetime(&cutoff)],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("file aggregate counts");
        assert_eq!(counts, (3, 1, 1));
    }

    #[test]
    fn hash_mismatch_detected() {
        let (store, fid) = store_with_file();
        // First hash run — baseline
        let first = VerificationRecord::new(
            Uuid::new_v4(),
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            0,
            Some("hash-a".into()),
            None,
        );
        // Second hash run — different content
        let second = VerificationRecord::new(
            Uuid::new_v4(),
            fid,
            Utc::now(),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Error,
            1,
            0,
            Some("hash-b".into()),
            Some(r#"{"prior_hash":"hash-a"}"#.into()),
        );
        store.insert_verification(&first).unwrap();
        store.insert_verification(&second).unwrap();
        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .unwrap();
        assert_eq!(summary.hash_mismatches, 1);
    }

    #[test]
    fn no_hash_mismatch_when_identical() {
        let (store, fid) = store_with_file();
        let first = VerificationRecord::new(
            Uuid::new_v4(),
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            0,
            Some("same-hash".into()),
            None,
        );
        let second = VerificationRecord::new(
            Uuid::new_v4(),
            fid,
            Utc::now(),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            0,
            Some("same-hash".into()),
            None,
        );
        store.insert_verification(&first).unwrap();
        store.insert_verification(&second).unwrap();
        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .unwrap();
        assert_eq!(summary.hash_mismatches, 0);
    }

    #[test]
    fn with_errors_uses_latest_only() {
        let (store, fid) = store_with_file();
        // Older error
        let earlier = VerificationRecord::new(
            Uuid::new_v4(),
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Quick,
            voom_domain::verification::VerificationOutcome::Error,
            1,
            0,
            None,
            None,
        );
        // Newer ok — should make this file NOT count as with_errors
        let later = VerificationRecord::new(
            Uuid::new_v4(),
            fid,
            Utc::now(),
            VerificationMode::Quick,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            0,
            None,
            None,
        );
        store.insert_verification(&earlier).unwrap();
        store.insert_verification(&later).unwrap();
        let summary = store
            .integrity_summary(Utc::now() - chrono::Duration::days(30))
            .unwrap();
        assert_eq!(summary.with_errors, 0);
        assert_eq!(summary.with_warnings, 0);
    }
}
