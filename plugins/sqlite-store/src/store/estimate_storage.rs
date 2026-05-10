//! `EstimateStorage` implementation backed by SQLite.

use rusqlite::{Row, params};
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::estimate::{CostModelSample, EstimateOperationKey, EstimateRun, FileEstimate};
use voom_domain::storage::{CostModelSampleFilters, EstimateStorage};

use super::{
    OptionalExt, SqlQuery, SqliteStore, checked_i64_to_u64, format_datetime, other_storage_err,
    parse_required_datetime, row_uuid, storage_err,
};

fn usize_to_i64(value: usize, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(other_storage_err(&format!("{field} does not fit in i64")))
}

fn u64_to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(other_storage_err(&format!("{field} does not fit in i64")))
}

fn i64_to_usize(value: i64, field: &str) -> rusqlite::Result<usize> {
    usize::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            format!("invalid {field} in estimate_runs: {error}").into(),
        )
    })
}

fn row_to_estimate_run_without_files(row: &Row<'_>) -> rusqlite::Result<EstimateRun> {
    let id: String = row.get("id")?;
    let estimated_at: String = row.get("estimated_at")?;
    let bytes_saved: i64 = row.get("bytes_saved")?;
    Ok(EstimateRun {
        id: row_uuid(&id, "estimate_runs")?,
        estimated_at: parse_required_datetime(estimated_at, "estimate_runs.estimated_at")?,
        file_count: i64_to_usize(row.get("file_count")?, "file_count")?,
        bytes_in: checked_i64_to_u64(row.get("bytes_in")?, "estimate_runs.bytes_in")?,
        bytes_out: checked_i64_to_u64(row.get("bytes_out")?, "estimate_runs.bytes_out")?,
        bytes_saved,
        compute_time_ms: checked_i64_to_u64(
            row.get("compute_time_ms")?,
            "estimate_runs.compute_time_ms",
        )?,
        wall_time_ms: checked_i64_to_u64(row.get("wall_time_ms")?, "estimate_runs.wall_time_ms")?,
        high_uncertainty_files: i64_to_usize(
            row.get("high_uncertainty_files")?,
            "high_uncertainty_files",
        )?,
        net_loss_files: i64_to_usize(row.get("net_loss_files")?, "net_loss_files")?,
        files: Vec::new(),
    })
}

fn row_to_cost_model_sample(row: &Row<'_>) -> rusqlite::Result<CostModelSample> {
    let id: String = row.get("id")?;
    let completed_at: String = row.get("completed_at")?;
    Ok(CostModelSample {
        id: row_uuid(&id, "cost_model_samples")?,
        key: EstimateOperationKey::transcode(
            row.get::<_, String>("phase_name")?,
            row.get::<_, String>("codec")?,
            row.get::<_, String>("preset")?,
            row.get::<_, String>("backend")?,
        ),
        pixels_per_second: row.get("pixels_per_second")?,
        output_size_ratio: row.get("output_size_ratio")?,
        fixed_overhead_ms: checked_i64_to_u64(
            row.get("fixed_overhead_ms")?,
            "cost_model_samples.fixed_overhead_ms",
        )?,
        completed_at: parse_required_datetime(completed_at, "cost_model_samples.completed_at")?,
    })
}

impl EstimateStorage for SqliteStore {
    fn insert_estimate_run(&self, run: &EstimateRun) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO estimate_runs \
             (id, estimated_at, file_count, bytes_in, bytes_out, bytes_saved, \
              compute_time_ms, wall_time_ms, high_uncertainty_files, net_loss_files) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run.id.to_string(),
                format_datetime(&run.estimated_at),
                usize_to_i64(run.file_count, "file_count")?,
                u64_to_i64(run.bytes_in, "bytes_in")?,
                u64_to_i64(run.bytes_out, "bytes_out")?,
                run.bytes_saved,
                u64_to_i64(run.compute_time_ms, "compute_time_ms")?,
                u64_to_i64(run.wall_time_ms, "wall_time_ms")?,
                usize_to_i64(run.high_uncertainty_files, "high_uncertainty_files")?,
                usize_to_i64(run.net_loss_files, "net_loss_files")?,
            ],
        )
        .map_err(storage_err("failed to insert estimate run"))?;
        insert_estimate_files(&conn, run)?;
        Ok(())
    }

    fn get_estimate_run(&self, id: &Uuid) -> Result<Option<EstimateRun>> {
        let conn = self.conn()?;
        let mut run = conn
            .query_row(
                "SELECT id, estimated_at, file_count, bytes_in, bytes_out, bytes_saved, \
                 compute_time_ms, wall_time_ms, high_uncertainty_files, net_loss_files \
                 FROM estimate_runs WHERE id = ?1",
                [id.to_string()],
                row_to_estimate_run_without_files,
            )
            .optional()
            .map_err(storage_err("failed to load estimate run"))?;
        if let Some(run) = run.as_mut() {
            run.files = load_estimate_files(&conn, &run.id)?;
        }
        Ok(run)
    }

    fn list_estimate_runs(&self, limit: u32) -> Result<Vec<EstimateRun>> {
        let conn = self.conn()?;
        let limit = limit.min(10_000);
        let mut stmt = conn
            .prepare(
                "SELECT id, estimated_at, file_count, bytes_in, bytes_out, bytes_saved, \
                 compute_time_ms, wall_time_ms, high_uncertainty_files, net_loss_files \
                 FROM estimate_runs ORDER BY estimated_at DESC, id DESC LIMIT ?1",
            )
            .map_err(storage_err("failed to prepare estimate run query"))?;
        let rows = stmt
            .query_map([i64::from(limit)], row_to_estimate_run_without_files)
            .map_err(storage_err("failed to query estimate runs"))?;
        let mut runs = Vec::new();
        for row in rows {
            let mut run = row.map_err(storage_err("failed to read estimate run row"))?;
            run.files = load_estimate_files(&conn, &run.id)?;
            runs.push(run);
        }
        Ok(runs)
    }

    fn insert_cost_model_sample(&self, sample: &CostModelSample) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO cost_model_samples \
             (id, phase_name, codec, preset, backend, pixels_per_second, output_size_ratio, \
              fixed_overhead_ms, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                sample.id.to_string(),
                sample.key.phase_name,
                sample.key.codec,
                sample.key.preset,
                sample.key.backend,
                sample.pixels_per_second,
                sample.output_size_ratio,
                u64_to_i64(sample.fixed_overhead_ms, "fixed_overhead_ms")?,
                format_datetime(&sample.completed_at),
            ],
        )
        .map_err(storage_err("failed to insert cost model sample"))?;
        Ok(())
    }

    fn list_cost_model_samples(
        &self,
        filters: &CostModelSampleFilters,
    ) -> Result<Vec<CostModelSample>> {
        let conn = self.conn()?;
        let mut query = SqlQuery::new(
            "SELECT id, phase_name, codec, preset, backend, pixels_per_second, \
             output_size_ratio, fixed_overhead_ms, completed_at \
             FROM cost_model_samples WHERE 1=1",
        );
        if let Some(key) = filters.key.as_ref() {
            query.condition(" AND phase_name = {}", key.phase_name.clone());
            query.condition(" AND codec = {}", key.codec.clone());
            query.condition(" AND preset = {}", key.preset.clone());
            query.condition(" AND backend = {}", key.backend.clone());
        }
        query.sql.push_str(" ORDER BY completed_at DESC, id DESC");
        query.paginate(filters.limit, None);

        let mut stmt = conn
            .prepare(&query.sql)
            .map_err(storage_err("failed to prepare cost model sample query"))?;
        let rows = stmt
            .query_map(query.param_refs().as_slice(), row_to_cost_model_sample)
            .map_err(storage_err("failed to query cost model samples"))?;
        let mut samples = Vec::new();
        for row in rows {
            samples.push(row.map_err(storage_err("failed to read cost model sample row"))?);
        }
        Ok(samples)
    }
}

fn insert_estimate_files(conn: &rusqlite::Connection, run: &EstimateRun) -> Result<()> {
    for (index, file) in run.files.iter().enumerate() {
        let file_json = serde_json::to_string(file)
            .map_err(other_storage_err("failed to serialize estimate file"))?;
        conn.execute(
            "INSERT INTO estimate_files (id, run_id, file_index, file_json) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::new_v4().to_string(),
                run.id.to_string(),
                usize_to_i64(index, "file_index")?,
                file_json,
            ],
        )
        .map_err(storage_err("failed to insert estimate file"))?;
    }
    Ok(())
}

fn load_estimate_files(conn: &rusqlite::Connection, run_id: &Uuid) -> Result<Vec<FileEstimate>> {
    let mut stmt = conn
        .prepare("SELECT file_json FROM estimate_files WHERE run_id = ?1 ORDER BY file_index")
        .map_err(storage_err("failed to prepare estimate file query"))?;
    let rows = stmt
        .query_map([run_id.to_string()], |row| {
            row.get::<_, String>("file_json")
        })
        .map_err(storage_err("failed to query estimate files"))?;
    let mut files = Vec::new();
    for row in rows {
        let file_json = row.map_err(storage_err("failed to read estimate file row"))?;
        let file = serde_json::from_str(&file_json)
            .map_err(other_storage_err("failed to deserialize estimate file"))?;
        files.push(file);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use uuid::Uuid;

    use voom_domain::estimate::{
        ActionEstimate, CostModelSample, EstimateOperationKey, EstimateRun, FileEstimate,
    };
    use voom_domain::plan::OperationType;
    use voom_domain::storage::{CostModelSampleFilters, EstimateStorage};

    use crate::store::SqliteStore;

    fn estimate_run() -> EstimateRun {
        let estimated_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 12, 0, 0)
            .single()
            .expect("valid estimate timestamp");
        EstimateRun {
            id: Uuid::from_u128(1),
            estimated_at,
            file_count: 1,
            bytes_in: 1_000,
            bytes_out: 600,
            bytes_saved: 400,
            compute_time_ms: 10_000,
            wall_time_ms: 5_000,
            high_uncertainty_files: 1,
            net_loss_files: 0,
            files: vec![FileEstimate {
                file_id: Uuid::from_u128(2),
                path: "/media/movie.mkv".into(),
                phase_name: "video".into(),
                bytes_in: 1_000,
                bytes_out: 600,
                bytes_saved: 400,
                compute_time_ms: 10_000,
                high_uncertainty: true,
                net_byte_loss: false,
                actions: vec![ActionEstimate {
                    operation: OperationType::TranscodeVideo,
                    codec: Some("hevc".into()),
                    backend: Some("nvenc".into()),
                    bytes_out: 600,
                    compute_time_ms: 10_000,
                    high_uncertainty: true,
                }],
            }],
        }
    }

    #[test]
    fn estimate_runs_round_trip_with_file_and_action_details() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let run = estimate_run();

        store.insert_estimate_run(&run).expect("insert estimate");
        let loaded = store
            .get_estimate_run(&run.id)
            .expect("load estimate")
            .expect("estimate exists");

        assert_eq!(loaded.id, run.id);
        assert_eq!(loaded.bytes_saved, 400);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[0].actions.len(), 1);
        assert_eq!(loaded.files[0].actions[0].backend.as_deref(), Some("nvenc"));
    }

    #[test]
    fn cost_model_samples_filter_by_operation_key() {
        let store = SqliteStore::in_memory().expect("in-memory store");
        let completed_at = chrono::Utc
            .with_ymd_and_hms(2026, 5, 10, 12, 0, 0)
            .single()
            .expect("valid sample timestamp");
        let matching_key = EstimateOperationKey::transcode("video", "hevc", "slow", "nvenc");
        let other_key = EstimateOperationKey::transcode("video", "av1", "slow", "software");
        let matching =
            CostModelSample::new(matching_key.clone(), 4_000_000.0, 0.42, 500, completed_at);
        let other = CostModelSample::new(other_key, 1_000_000.0, 0.50, 500, completed_at);

        store
            .insert_cost_model_sample(&matching)
            .expect("insert matching sample");
        store
            .insert_cost_model_sample(&other)
            .expect("insert other sample");

        let filters = CostModelSampleFilters {
            key: Some(matching_key),
            limit: Some(10),
        };
        let samples = store
            .list_cost_model_samples(&filters)
            .expect("list samples");

        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].codec(), "hevc");
    }
}
