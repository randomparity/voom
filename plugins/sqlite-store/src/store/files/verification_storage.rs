//! `VerificationStorage` implementation backed by SQLite.

use chrono::{DateTime, Utc};
use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::storage::VerificationStorage;
use voom_domain::verification::{
    IntegritySummary, IntegritySummaryCounts, VerificationFilters, VerificationMode,
    VerificationRecord,
};

use crate::store::{format_datetime, row_to_verification, storage_err, SqlQuery, SqliteStore};

const SELECT_VERIFICATION: &str = "SELECT id, file_id, verified_at, mode, outcome, \
    error_count, warning_count, content_hash, details FROM verifications";

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
            q.parameterized_clause(" AND file_id = {}", file_id.clone());
        }
        if let Some(mode) = filters.mode {
            q.parameterized_clause(" AND mode = {}", mode.as_str().to_string());
        }
        if let Some(outcome) = filters.outcome {
            q.parameterized_clause(" AND outcome = {}", outcome.as_str().to_string());
        }
        if let Some(since) = filters.since.as_ref() {
            q.parameterized_clause(" AND verified_at >= {}", format_datetime(since));
        }
        q.sql.push_str(" ORDER BY verified_at DESC");
        q.paginate(filters.limit, filters.offset);

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

        let total_files: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE status = 'active'",
                [],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count files"))?;

        let never_verified: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files f \
                 WHERE f.status = 'active' AND NOT EXISTS ( \
                     SELECT 1 FROM verifications v WHERE v.file_id = f.id \
                 )",
                [],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count never-verified files"))?;

        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ( \
                     SELECT f.id, MAX(v.verified_at) AS last_at \
                     FROM files f \
                     JOIN verifications v ON v.file_id = f.id \
                     WHERE f.status = 'active' \
                     GROUP BY f.id \
                     HAVING last_at < ?1 \
                 )",
                params![format_datetime(&since)],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count stale files"))?;

        let with_errors: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_id) FROM ( \
                     SELECT file_id, outcome, \
                         ROW_NUMBER() OVER ( \
                             PARTITION BY file_id ORDER BY verified_at DESC, id DESC \
                         ) AS rn \
                     FROM verifications \
                 ) WHERE rn = 1 AND outcome = 'error'",
                [],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count files with errors"))?;

        let with_warnings: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_id) FROM ( \
                     SELECT file_id, outcome, \
                         ROW_NUMBER() OVER ( \
                             PARTITION BY file_id ORDER BY verified_at DESC, id DESC \
                         ) AS rn \
                     FROM verifications \
                 ) WHERE rn = 1 AND outcome = 'warning'",
                [],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count files with warnings"))?;

        let hash_mismatches: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT file_id) FROM ( \
                     SELECT file_id, content_hash, \
                         LAG(content_hash) OVER ( \
                             PARTITION BY file_id ORDER BY verified_at \
                         ) AS prev_hash, \
                         ROW_NUMBER() OVER ( \
                             PARTITION BY file_id ORDER BY verified_at DESC \
                         ) AS rn \
                     FROM verifications WHERE mode = 'hash' \
                 ) WHERE rn = 1 AND prev_hash IS NOT NULL AND prev_hash <> content_hash",
                [],
                |r| r.get(0),
            )
            .map_err(storage_err("failed to count hash mismatches"))?;

        Ok(IntegritySummaryCounts {
            total_files: u64::try_from(total_files.max(0)).unwrap_or(0),
            never_verified: u64::try_from(never_verified.max(0)).unwrap_or(0),
            stale: u64::try_from(stale.max(0)).unwrap_or(0),
            with_errors: u64::try_from(with_errors.max(0)).unwrap_or(0),
            with_warnings: u64::try_from(with_warnings.max(0)).unwrap_or(0),
            hash_mismatches: u64::try_from(hash_mismatches.max(0)).unwrap_or(0),
        }
        .into())
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
    use voom_domain::verification::{VerificationOutcome, VerificationRecordInput};

    fn store_with_file() -> (SqliteStore, String) {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let mut file = MediaFile::new(PathBuf::from("/media/test.mkv"));
        file.size = 1;
        file.content_hash = Some("hash-stub".into());
        file.container = Container::Mkv;
        file.introspected_at = Utc::now();
        store.upsert_file(&file).expect("insert file");
        (store, file.id.to_string())
    }

    fn verification_record(
        file_id: String,
        verified_at: chrono::DateTime<Utc>,
        mode: VerificationMode,
        outcome: VerificationOutcome,
        error_count: u32,
        content_hash: Option<String>,
        details: Option<String>,
    ) -> VerificationRecord {
        VerificationRecord::new(VerificationRecordInput {
            id: Uuid::new_v4(),
            file_id,
            verified_at,
            mode,
            outcome,
            error_count,
            warning_count: 0,
            content_hash,
            details,
        })
    }

    #[test]
    fn insert_and_list_verification() {
        let (store, file_id) = store_with_file();
        let record = VerificationRecord::new(VerificationRecordInput {
            id: Uuid::new_v4(),
            file_id: file_id.clone(),
            verified_at: Utc::now(),
            mode: VerificationMode::Quick,
            outcome: VerificationOutcome::Ok,
            error_count: 0,
            warning_count: 0,
            content_hash: None,
            details: None,
        });
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
        let earlier = VerificationRecord::new(VerificationRecordInput {
            id: Uuid::new_v4(),
            file_id: file_id.clone(),
            verified_at: Utc::now() - chrono::Duration::hours(1),
            mode: VerificationMode::Hash,
            outcome: VerificationOutcome::Ok,
            error_count: 0,
            warning_count: 0,
            content_hash: Some("hash-a".into()),
            details: None,
        });
        let later = VerificationRecord::new(VerificationRecordInput {
            id: Uuid::new_v4(),
            file_id: file_id.clone(),
            verified_at: Utc::now(),
            mode: VerificationMode::Hash,
            outcome: VerificationOutcome::Ok,
            error_count: 0,
            warning_count: 0,
            content_hash: Some("hash-b".into()),
            details: None,
        });
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
    fn hash_mismatch_detected() {
        let (store, fid) = store_with_file();
        // First hash run — baseline
        let first = verification_record(
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            Some("hash-a".into()),
            None,
        );
        // Second hash run — different content
        let second = verification_record(
            fid,
            Utc::now(),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Error,
            1,
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
        let first = verification_record(
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
            0,
            Some("same-hash".into()),
            None,
        );
        let second = verification_record(
            fid,
            Utc::now(),
            VerificationMode::Hash,
            voom_domain::verification::VerificationOutcome::Ok,
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
        let earlier = verification_record(
            fid.clone(),
            Utc::now() - chrono::Duration::hours(2),
            VerificationMode::Quick,
            voom_domain::verification::VerificationOutcome::Error,
            1,
            None,
            None,
        );
        // Newer ok — should make this file NOT count as with_errors
        let later = verification_record(
            fid,
            Utc::now(),
            VerificationMode::Quick,
            voom_domain::verification::VerificationOutcome::Ok,
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
