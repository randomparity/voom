//! `TranscodeOutcomeStorage` implementation backed by SQLite.

use rusqlite::{params, Row};

use voom_domain::errors::Result;
use voom_domain::storage::{TranscodeOutcomeFilters, TranscodeOutcomeStorage};
use voom_domain::transcode::TranscodeOutcome;

use super::{format_datetime, parse_required_datetime, row_uuid, storage_err, SqlQuery};
use super::{other_storage_err, SqliteStore};

const SELECT_TRANSCODE_OUTCOME: &str = "SELECT id, file_id, target_vmaf, achieved_vmaf, \
    crf_used, bitrate_used, iterations, sample_strategy, fallback_used, completed_at \
    FROM transcode_outcomes";

fn optional_u32(row: &Row<'_>, column: &str) -> rusqlite::Result<Option<u32>> {
    row.get::<_, Option<i64>>(column)?
        .map(|value| {
            u32::try_from(value).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    format!("invalid {column} in transcode_outcomes: {e}").into(),
                )
            })
        })
        .transpose()
}

fn row_to_transcode_outcome(row: &Row<'_>) -> rusqlite::Result<TranscodeOutcome> {
    let id: String = row.get("id")?;
    let sample_strategy: String = row.get("sample_strategy")?;
    let completed_at: String = row.get("completed_at")?;
    let iterations = u32::try_from(row.get::<_, i64>("iterations")?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            format!("invalid iterations in transcode_outcomes: {e}").into(),
        )
    })?;
    let sample_strategy = serde_json::from_str(&sample_strategy).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("invalid JSON in transcode_outcomes.sample_strategy: {e}").into(),
        )
    })?;

    Ok(TranscodeOutcome {
        id: row_uuid(&id, "transcode_outcomes")?,
        file_id: row.get("file_id")?,
        target_vmaf: optional_u32(row, "target_vmaf")?,
        achieved_vmaf: row
            .get::<_, Option<f64>>("achieved_vmaf")?
            .map(|v| v as f32),
        crf_used: optional_u32(row, "crf_used")?,
        bitrate_used: row.get("bitrate_used")?,
        iterations,
        sample_strategy,
        fallback_used: row.get::<_, i64>("fallback_used")? != 0,
        completed_at: parse_required_datetime(completed_at, "transcode_outcomes.completed_at")?,
    })
}

impl TranscodeOutcomeStorage for SqliteStore {
    fn insert_transcode_outcome(&self, outcome: &TranscodeOutcome) -> Result<()> {
        let conn = self.conn()?;
        let sample_strategy = serde_json::to_string(&outcome.sample_strategy)
            .map_err(other_storage_err("failed to serialize sample strategy"))?;
        conn.execute(
            "INSERT INTO transcode_outcomes \
             (id, file_id, target_vmaf, achieved_vmaf, crf_used, bitrate_used, iterations, \
              sample_strategy, fallback_used, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                outcome.id.to_string(),
                outcome.file_id,
                outcome.target_vmaf.map(i64::from),
                outcome.achieved_vmaf.map(f64::from),
                outcome.crf_used.map(i64::from),
                outcome.bitrate_used,
                i64::from(outcome.iterations),
                sample_strategy,
                i64::from(outcome.fallback_used),
                format_datetime(&outcome.completed_at),
            ],
        )
        .map_err(storage_err("failed to insert transcode outcome"))?;
        Ok(())
    }

    fn list_transcode_outcomes(
        &self,
        filters: &TranscodeOutcomeFilters,
    ) -> Result<Vec<TranscodeOutcome>> {
        let conn = self.conn()?;
        let mut q = SqlQuery::new(&format!("{SELECT_TRANSCODE_OUTCOME} WHERE 1=1"));
        if let Some(file_id) = filters.file_id.as_ref() {
            q.condition(" AND file_id = {}", file_id.clone());
        }
        q.sql.push_str(" ORDER BY completed_at DESC, id DESC");
        q.paginate(filters.limit, None);

        let mut stmt = conn
            .prepare(&q.sql)
            .map_err(storage_err("failed to prepare transcode outcome query"))?;
        let rows = stmt
            .query_map(q.param_refs().as_slice(), row_to_transcode_outcome)
            .map_err(storage_err("failed to query transcode outcomes"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(storage_err("failed to read transcode outcome row"))?);
        }
        Ok(out)
    }

    fn latest_outcome_for_file(&self, file_id: &str) -> Result<Option<TranscodeOutcome>> {
        let mut filters = TranscodeOutcomeFilters::default();
        filters.file_id = Some(file_id.to_string());
        filters.limit = Some(1);
        let mut outcomes = self.list_transcode_outcomes(&filters)?;
        Ok(outcomes.pop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::PathBuf;
    use uuid::Uuid;
    use voom_domain::media::{Container, MediaFile};
    use voom_domain::plan::SampleStrategy;
    use voom_domain::storage::FileStorage;
    use voom_domain::test_support::InMemoryStore;

    fn make_file(path: &str) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path));
        file.size = 1;
        file.content_hash = Some("hash-stub".to_string());
        file.container = Container::Mkv;
        file.introspected_at = Utc::now();
        file
    }

    fn outcome(id: u128, file_id: &str, completed_at: chrono::DateTime<Utc>) -> TranscodeOutcome {
        TranscodeOutcome {
            id: Uuid::from_u128(id),
            file_id: file_id.to_string(),
            target_vmaf: Some(95),
            achieved_vmaf: Some(94.8),
            crf_used: Some(22),
            bitrate_used: Some("3200k".to_string()),
            iterations: 3,
            sample_strategy: SampleStrategy::Scenes {
                count: 8,
                duration: "12s".to_string(),
            },
            fallback_used: false,
            completed_at,
        }
    }

    #[test]
    fn insert_and_list_transcode_outcomes_round_trip() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let file = make_file("/media/test.mkv");
        let file_id = file.id.to_string();
        store.upsert_file(&file).expect("insert file");
        let record = outcome(1, &file_id, Utc::now());

        store
            .insert_transcode_outcome(&record)
            .expect("insert outcome");
        let mut filters = TranscodeOutcomeFilters::default();
        filters.file_id = Some(file_id);
        let listed = store
            .list_transcode_outcomes(&filters)
            .expect("list outcomes");

        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn latest_outcome_for_file_returns_newest_with_id_tiebreaker() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let file = make_file("/media/test.mkv");
        let file_id = file.id.to_string();
        let completed_at = Utc::now();
        store.upsert_file(&file).expect("insert file");

        for record in [
            outcome(1, &file_id, completed_at),
            outcome(2, &file_id, completed_at),
        ] {
            store
                .insert_transcode_outcome(&record)
                .expect("insert outcome");
        }

        let latest = store
            .latest_outcome_for_file(&file_id)
            .expect("latest")
            .expect("some outcome");
        assert_eq!(latest.id, Uuid::from_u128(2));
    }

    #[test]
    fn transcode_outcome_storage_matches_in_memory_store() {
        let sqlite = SqliteStore::in_memory().expect("in-memory sqlite");
        let memory = InMemoryStore::new();
        let file = make_file("/media/test.mkv");
        let file_id = file.id.to_string();
        let completed_at = Utc::now();
        sqlite.upsert_file(&file).expect("sqlite insert file");
        memory.upsert_file(&file).expect("memory insert file");

        for record in [
            outcome(1, &file_id, completed_at),
            outcome(3, &file_id, completed_at),
            outcome(2, &file_id, completed_at - chrono::Duration::minutes(1)),
        ] {
            sqlite
                .insert_transcode_outcome(&record)
                .expect("sqlite insert outcome");
            memory
                .insert_transcode_outcome(&record)
                .expect("memory insert outcome");
        }
        let mut filters = TranscodeOutcomeFilters::default();
        filters.file_id = Some(file_id);

        assert_eq!(
            sqlite
                .list_transcode_outcomes(&filters)
                .expect("sqlite outcomes"),
            memory
                .list_transcode_outcomes(&filters)
                .expect("memory outcomes")
        );
    }
}
