use std::path::PathBuf;

use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::stats::ProcessingOutcome;
use voom_domain::storage::FileTransitionStorage;
use voom_domain::transition::{FileTransition, TransitionSource};

use super::{format_datetime, storage_err, SqliteStore};

impl FileTransitionStorage for SqliteStore {
    fn record_transition(&self, t: &FileTransition) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO file_transitions \
             (id, file_id, path, from_hash, to_hash, from_size, to_size, \
              source, source_detail, plan_id, \
              duration_ms, actions_taken, tracks_modified, outcome, \
              policy_name, phase_name, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
                     ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                t.id.to_string(),
                t.file_id.to_string(),
                t.path.to_string_lossy().to_string(),
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
                format_datetime(&t.created_at),
            ],
        )
        .map_err(storage_err("failed to record transition"))?;
        Ok(())
    }

    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, created_at \
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
                "SELECT id, file_id, path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, created_at \
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

    fn transitions_for_path(&self, path: &std::path::Path) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_hash, to_hash, from_size, to_size, \
                 source, source_detail, plan_id, \
                 duration_ms, actions_taken, tracks_modified, outcome, \
                 policy_name, phase_name, created_at \
                 FROM file_transitions WHERE path = ?1 ORDER BY created_at ASC",
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
}

fn row_to_transition(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTransition> {
    let id_str: String = row.get("id")?;
    let file_id_str: String = row.get("file_id")?;
    let path_str: String = row.get("path")?;
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
    let source = TransitionSource::parse(&source_str).unwrap_or_default();
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

    let mut t = FileTransition::new(
        file_id,
        PathBuf::from(path_str),
        to_hash,
        to_size as u64,
        source,
    );
    t.id = id;
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
