mod bad_file_storage;
mod discovered_file_storage;
mod estimate_storage;
mod event_log_storage;
mod file_storage;
mod file_transition_storage;
mod health_check_storage;
mod job_storage;
mod maintenance_storage;
mod pending_ops_storage;
mod plan_storage;
mod plugin_data_storage;
mod row_mappers;
mod snapshot_storage;
mod subtitle_storage;
mod transcode_outcome_storage;
mod verification_storage;

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::{Result, StorageErrorKind, VoomError};
use voom_domain::media::Track;

pub(crate) use row_mappers::{
    checked_i64_to_u64, checked_optional_i64_to_u32, checked_optional_i64_to_u64,
    parse_optional_datetime, parse_required_datetime, row_to_bad_file, row_to_file, row_to_job,
    row_to_track, row_uuid, FileRow,
};

use crate::schema;

/// Configuration for the `SQLite` store.
pub struct SqliteStoreConfig {
    /// Maximum number of connections in the pool. Default: 8.
    pub pool_size: u32,
}

impl Default for SqliteStoreConfig {
    fn default() -> Self {
        Self { pool_size: 8 }
    }
}

/// SQLite-backed storage implementation using r2d2 connection pooling.
pub struct SqliteStore {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteStore {
    /// Open (or create) a `SQLite` database at the given path.
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::open_with_config(db_path, SqliteStoreConfig::default())
    }

    /// Open with custom configuration.
    pub fn open_with_config(db_path: &Path, config: SqliteStoreConfig) -> Result<Self> {
        let manager = SqliteConnectionManager::file(db_path);
        Self::from_manager(manager, config.pool_size)
    }

    /// Create an in-memory `SQLite` store (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let manager = SqliteConnectionManager::memory();
        Self::from_manager(manager, SqliteStoreConfig::default().pool_size)
    }

    fn from_manager(manager: SqliteConnectionManager, pool_size: u32) -> Result<Self> {
        // Configure every connection from the pool with pragmas (WAL, busy_timeout, etc.)
        let manager = manager.with_init(|conn| schema::configure_connection(conn));

        let pool = Pool::builder()
            .max_size(pool_size)
            .min_idle(Some(0))
            .build(manager)
            .map_err(other_storage_err("failed to create connection pool"))?;

        // Initialize schema on the first connection
        let conn = pool
            .get()
            .map_err(other_storage_err("failed to get connection"))?;
        schema::create_schema(&conn).map_err(storage_err("failed to create schema"))?;

        Ok(Self { pool })
    }

    pub(crate) fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(other_storage_err("failed to get connection"))
    }
}

/// Classify a rusqlite error into a [`StorageErrorKind`].
fn classify_rusqlite(e: &rusqlite::Error) -> StorageErrorKind {
    match e {
        rusqlite::Error::SqliteFailure(ffi_err, _) => {
            use rusqlite::ffi::ErrorCode;
            match ffi_err.code {
                ErrorCode::ConstraintViolation => StorageErrorKind::ConstraintViolation,
                _ => StorageErrorKind::Other,
            }
        }
        rusqlite::Error::QueryReturnedNoRows => StorageErrorKind::NotFound,
        _ => StorageErrorKind::Other,
    }
}

/// Create a `.map_err` closure for `rusqlite::Error` that classifies the error kind.
pub(crate) fn storage_err(msg: &str) -> impl FnOnce(rusqlite::Error) -> VoomError + '_ {
    move |e| VoomError::Storage {
        kind: classify_rusqlite(&e),
        message: format!("{msg}: {e}"),
    }
}

/// Wrap any displayable error as a generic storage error with [`StorageErrorKind::Other`].
pub(crate) fn other_storage_err<E: std::fmt::Display>(
    msg: &str,
) -> impl FnOnce(E) -> VoomError + '_ {
    move |e| VoomError::Storage {
        kind: StorageErrorKind::Other,
        message: format!("{msg}: {e}"),
    }
}

/// Lightweight builder for dynamic SQL queries with positional parameters.
pub(crate) struct SqlQuery {
    pub(crate) sql: String,
    params: Vec<String>,
}

impl SqlQuery {
    pub(crate) fn new(base: &str) -> Self {
        Self {
            sql: base.to_string(),
            params: Vec::new(),
        }
    }

    /// Add a condition with a parameter value. Returns `&mut Self` for chaining.
    pub(crate) fn condition(&mut self, clause: &str, value: String) -> &mut Self {
        self.params.push(value);
        self.sql
            .push_str(&clause.replace("{}", &format!("?{}", self.params.len())));
        self
    }

    /// Append LIMIT and OFFSET clauses with clamped values.
    pub(crate) fn paginate(&mut self, limit: Option<u32>, offset: Option<u32>) {
        if let Some(limit) = limit {
            self.condition(" LIMIT {}", limit.min(10_000).to_string());
        }
        if let Some(offset) = offset {
            self.condition(" OFFSET {}", offset.min(1_000_000).to_string());
        }
    }

    /// Build the parameter references for rusqlite.
    pub(crate) fn param_refs(&self) -> Vec<&dyn rusqlite::types::ToSql> {
        self.params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect()
    }
}

pub(crate) fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(other_storage_err(&format!("invalid UUID '{s}'")))
}

/// Escape LIKE wildcard characters so user-supplied strings match literally.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(other_storage_err(&format!("invalid datetime '{s}'")))
}

pub(crate) fn format_datetime(dt: &DateTime<Utc>) -> String {
    voom_domain::utils::format::format_iso(dt)
}

/// Extension trait for `rusqlite::Result<T>` to convert to `Option<T>`.
pub(crate) trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Allowed (table, column) pairs for bulk deletion.
/// Each variant maps to exactly one static pair, making SQL
/// interpolation safe by construction.
pub(crate) enum PruneTarget {
    BadFiles,
}

impl PruneTarget {
    fn table(&self) -> &'static str {
        match self {
            Self::BadFiles => "bad_files",
        }
    }

    fn column(&self) -> &'static str {
        match self {
            Self::BadFiles => "id",
        }
    }
}

// Private helper methods
impl SqliteStore {
    /// Delete rows matching `target`'s (table, column) where the column
    /// value is in `ids`, processing in chunks of 500.
    /// Returns the total number of rows deleted.
    pub(crate) fn chunked_delete(&self, target: PruneTarget, ids: &[&str]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let table = target.table();
        let column = target.column();
        let conn = self.conn()?;
        let mut total = 0u64;
        for chunk in ids.chunks(500) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let in_clause = placeholders.join(",");
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            let deleted = conn
                .execute(
                    &format!("DELETE FROM {table} WHERE {column} IN ({in_clause})"),
                    param_refs.as_slice(),
                )
                .map_err(storage_err(&format!("failed to delete from {table}")))?;
            total += deleted as u64;
        }
        Ok(total)
    }

    pub(crate) fn load_tracks_batch(
        &self,
        conn: &rusqlite::Connection,
        file_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<Track>>> {
        let mut result: HashMap<Uuid, Vec<Track>> = HashMap::new();
        if file_ids.is_empty() {
            return Ok(result);
        }

        for chunk in file_ids.chunks(500) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT file_id, stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, loudness_integrated_lufs, loudness_true_peak_db, loudness_range_lu, loudness_measured_at, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format, color_primaries, color_transfer, color_matrix, max_cll, max_fall, master_display, dolby_vision_profile, is_animation \
                 FROM tracks WHERE file_id IN ({}) ORDER BY file_id, stream_index",
                placeholders.join(",")
            );
            let param_values: Vec<String> =
                chunk.iter().map(std::string::ToString::to_string).collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
                .iter()
                .map(|v| v as &dyn rusqlite::types::ToSql)
                .collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(storage_err("failed to prepare batch track query"))?;

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let file_id_str: String = row.get("file_id")?;
                    let track = row_to_track(row)?;
                    Ok((file_id_str, track))
                })
                .map_err(storage_err("failed to batch query tracks"))?;

            for row_result in rows {
                let (file_id_str, track) =
                    row_result.map_err(storage_err("failed to read track row"))?;
                let file_id = parse_uuid(&file_id_str)?;
                result.entry(file_id).or_default().push(track);
            }
        }

        Ok(result)
    }

    pub(crate) fn load_tracks(
        &self,
        conn: &rusqlite::Connection,
        file_id: &Uuid,
    ) -> Result<Vec<Track>> {
        let mut stmt = conn
            .prepare(
                "SELECT stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, loudness_integrated_lufs, loudness_true_peak_db, loudness_range_lu, loudness_measured_at, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format, color_primaries, color_transfer, color_matrix, max_cll, max_fall, master_display, dolby_vision_profile, is_animation
                 FROM tracks WHERE file_id = ?1 ORDER BY stream_index",
            )
            .map_err(storage_err("failed to prepare track query"))?;

        let tracks = stmt
            .query_map(params![file_id.to_string()], row_to_track)
            .map_err(storage_err("failed to query tracks"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect tracks"))?;

        Ok(tracks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::bad_file::{BadFile, BadFileSource};
    use voom_domain::job::{Job, JobStatus};
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::{OperationType, PlannedAction};
    use voom_domain::stats::ProcessingOutcome;
    use voom_domain::storage::{
        BadFileFilters, BadFileStorage, FileFilters, FileStorage, FileTransitionStorage,
        JobFilters, JobStorage, MaintenanceStorage, PlanStatus, PlanStorage, PluginDataStorage,
    };
    use voom_domain::transition::{FileTransition, TransitionSource};

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    fn sample_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/movies/test.mkv"));
        file.size = 1_500_000_000;
        file.content_hash = Some("abc123def456".to_string());
        file.container = Container::Mkv;
        file.duration = 7200.0;
        file.bitrate = Some(8000);
        let mut video = Track::new(0, TrackType::Video, "hevc".into());
        video.is_animation = Some(true);
        video.is_hdr = true;
        video.hdr_format = Some("HDR10".into());
        video.color_primaries = Some("bt2020".into());
        video.color_transfer = Some("smpte2084".into());
        video.color_matrix = Some("bt2020nc".into());
        video.max_cll = Some(1000);
        video.max_fall = Some(400);
        video.master_display = Some("G(1,2)B(3,4)R(5,6)WP(7,8)L(100,1)".into());
        file.tracks = vec![
            video,
            {
                let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
                t.language = "eng".into();
                t.is_default = true;
                t.channels = Some(6);
                t.channel_layout = Some("5.1".into());
                t.sample_rate = Some(48000);
                t
            },
            {
                let mut t = Track::new(2, TrackType::SubtitleMain, "srt".into());
                t.language = "eng".into();
                t
            },
        ];
        file
    }

    // --- File CRUD ---

    #[test]
    fn test_upsert_and_get_file() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let loaded = store.file(&file.id).unwrap().unwrap();
        assert_eq!(loaded.id, file.id);
        assert_eq!(loaded.path, file.path);
        assert_eq!(loaded.size, file.size);
        assert_eq!(loaded.content_hash, file.content_hash);
        assert_eq!(loaded.container, Container::Mkv);
        assert_eq!(loaded.duration, 7200.0);
        assert_eq!(loaded.tracks.len(), 3);
    }

    #[test]
    fn test_get_file_tracks_preserved() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let loaded = store.file(&file.id).unwrap().unwrap();
        let video = &loaded.tracks[0];
        assert_eq!(video.is_animation, Some(true));
        assert_eq!(video.hdr_format.as_deref(), Some("HDR10"));
        assert_eq!(video.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(video.color_transfer.as_deref(), Some("smpte2084"));
        assert_eq!(video.color_matrix.as_deref(), Some("bt2020nc"));
        assert_eq!(video.max_cll, Some(1000));
        assert_eq!(video.max_fall, Some(400));
        assert_eq!(
            video.master_display.as_deref(),
            Some("G(1,2)B(3,4)R(5,6)WP(7,8)L(100,1)")
        );
        let audio = &loaded.tracks[1];
        assert_eq!(audio.codec, "aac");
        assert_eq!(audio.language, "eng");
        assert!(audio.is_default);
        assert_eq!(audio.channels, Some(6));
        assert_eq!(audio.channel_layout.as_deref(), Some("5.1"));
        assert_eq!(audio.sample_rate, Some(48000));
    }

    #[test]
    fn test_get_file_by_path() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let loaded = store
            .file_by_path(Path::new("/media/movies/test.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id, file.id);
    }

    #[test]
    fn test_get_nonexistent_file() {
        let store = test_store();
        let result = store.file(&Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_upsert_updates_existing() {
        let store = test_store();
        let mut file = sample_file();
        store.upsert_file(&file).unwrap();

        file.size = 2_000_000_000;
        file.tracks
            .push(Track::new(3, TrackType::AudioCommentary, "aac".into()));
        store.upsert_file(&file).unwrap();

        let loaded = store.file(&file.id).unwrap().unwrap();
        assert_eq!(loaded.size, 2_000_000_000);
        assert_eq!(loaded.tracks.len(), 4);
    }

    #[test]
    fn test_mark_missing() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();
        store.mark_missing(&file.id).unwrap();

        // File should still exist but status is missing
        let loaded = store.file(&file.id).unwrap();
        // mark_missing is a stub — file still present until purge
        assert!(loaded.is_some());
    }

    #[test]
    fn test_list_files_no_filter() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let files = store.list_files(&FileFilters::default()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, file.id);
    }

    #[test]
    fn test_list_files_with_container_filter() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut file2 = MediaFile::new(PathBuf::from("/media/test2.mp4"));
        file2.container = Container::Mp4;
        file2.content_hash = Some("xyz".into());
        store.upsert_file(&file2).unwrap();

        let filters = {
            let mut f = FileFilters::default();
            f.container = Some(Container::Mkv);
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].container, Container::Mkv);
    }

    #[test]
    fn test_list_files_with_path_prefix() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut file2 = MediaFile::new(PathBuf::from("/other/test2.mkv"));
        file2.content_hash = Some("xyz".into());
        store.upsert_file(&file2).unwrap();

        let filters = {
            let mut f = FileFilters::default();
            f.path_prefix = Some("/media".into());
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_list_files_with_codec_filter() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let filters = {
            let mut f = FileFilters::default();
            f.has_codec = Some("hevc".into());
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);

        let filters = {
            let mut f = FileFilters::default();
            f.has_codec = Some("av1".into());
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_list_files_limit_offset() {
        let store = test_store();
        for i in 0..5 {
            let mut file = MediaFile::new(PathBuf::from(format!("/media/file{i}.mkv")));
            file.content_hash = Some(format!("hash{i}"));
            store.upsert_file(&file).unwrap();
        }

        let filters = {
            let mut f = FileFilters::default();
            f.limit = Some(2);
            f.offset = Some(1);
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 2);
    }

    // --- Job CRUD ---

    #[test]
    fn test_create_and_get_job() {
        let store = test_store();
        let mut job = Job::new(voom_domain::job::JobType::Transcode);
        job.priority = 50;
        job.payload = Some(serde_json::json!({"file": "/test.mkv"}));

        let id = store.create_job(&job).unwrap();
        assert_eq!(id, job.id);

        let loaded = store.job(&id).unwrap().unwrap();
        assert_eq!(loaded.job_type, voom_domain::job::JobType::Transcode);
        assert_eq!(loaded.priority, 50);
        assert_eq!(loaded.status, JobStatus::Pending);
    }

    #[test]
    fn test_update_job() {
        let store = test_store();
        let job = Job::new(voom_domain::job::JobType::Scan);
        store.create_job(&job).unwrap();

        let mut update = voom_domain::job::JobUpdate::default();
        update.status = Some(JobStatus::Running);
        update.progress = Some(0.5);
        update.progress_message = Some(Some("Scanning...".into()));
        update.worker_id = Some(Some("worker-1".into()));
        update.started_at = Some(Some(Utc::now()));
        store.update_job(&job.id, &update).unwrap();

        let loaded = store.job(&job.id).unwrap().unwrap();
        assert_eq!(loaded.status, JobStatus::Running);
        assert_eq!(loaded.progress, 0.5);
        assert_eq!(loaded.progress_message.as_deref(), Some("Scanning..."));
        assert_eq!(loaded.worker_id.as_deref(), Some("worker-1"));
        assert!(loaded.started_at.is_some());
    }

    #[test]
    fn test_claim_next_job() {
        let store = test_store();

        let mut job1 = Job::new(voom_domain::job::JobType::Custom("task1".into()));
        job1.priority = 200;
        store.create_job(&job1).unwrap();

        let mut job2 = Job::new(voom_domain::job::JobType::Custom("task2".into()));
        job2.priority = 50; // higher priority (lower number)
        store.create_job(&job2).unwrap();

        let claimed = store.claim_next_job("worker-1").unwrap().unwrap();
        assert_eq!(
            claimed.job_type,
            voom_domain::job::JobType::Custom("task2".into())
        ); // lower priority number = claimed first
        assert_eq!(claimed.status, JobStatus::Running);
        assert_eq!(claimed.worker_id.as_deref(), Some("worker-1"));
    }

    #[test]
    fn test_list_jobs() {
        let store = test_store();
        let mut job1 = Job::new(voom_domain::job::JobType::Custom("task1".into()));
        job1.priority = 100;
        store.create_job(&job1).unwrap();

        let mut job2 = Job::new(voom_domain::job::JobType::Custom("task2".into()));
        job2.priority = 50;
        store.create_job(&job2).unwrap();

        // Claim one to make it running
        store.claim_next_job("w-1").unwrap();

        let all = store.list_jobs(&JobFilters::default()).unwrap();
        assert_eq!(all.len(), 2);

        let pending = store
            .list_jobs(&{
                let mut f = JobFilters::default();
                f.status = Some(JobStatus::Pending);
                f.limit = None;
                f
            })
            .unwrap();
        assert_eq!(pending.len(), 1);

        let running = store
            .list_jobs(&{
                let mut f = JobFilters::default();
                f.status = Some(JobStatus::Running);
                f.limit = None;
                f
            })
            .unwrap();
        assert_eq!(running.len(), 1);

        let limited = store
            .list_jobs(&{
                let mut f = JobFilters::default();
                f.status = None;
                f.limit = Some(1);
                f
            })
            .unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_count_jobs_by_status() {
        let store = test_store();
        for i in 0..3 {
            let job = Job::new(voom_domain::job::JobType::Custom(format!("task{i}")));
            store.create_job(&job).unwrap();
        }
        store.claim_next_job("w-1").unwrap();

        let counts = store.count_jobs_by_status().unwrap();
        let pending = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Pending)
            .map_or(0, |(_, c)| *c);
        let running = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Running)
            .map_or(0, |(_, c)| *c);
        assert_eq!(pending, 2);
        assert_eq!(running, 1);
    }

    #[test]
    fn test_claim_next_job_with_existing_running_job() {
        let store = test_store();

        // Worker already has a running job
        let mut running_job = Job::new(voom_domain::job::JobType::Custom("running".into()));
        running_job.priority = 100;
        store.create_job(&running_job).unwrap();
        store.claim_next_job("worker-1").unwrap();

        // Add two more pending jobs
        let mut job_high = Job::new(voom_domain::job::JobType::Custom("high-pri".into()));
        job_high.priority = 10;
        store.create_job(&job_high).unwrap();

        let mut job_low = Job::new(voom_domain::job::JobType::Custom("low-pri".into()));
        job_low.priority = 200;
        store.create_job(&job_low).unwrap();

        // Same worker claims next — should get the highest-priority pending job,
        // not the already-running one
        let claimed = store.claim_next_job("worker-1").unwrap().unwrap();
        assert_eq!(
            claimed.job_type,
            voom_domain::job::JobType::Custom("high-pri".into())
        );
        assert_eq!(claimed.id, job_high.id);
        assert_eq!(claimed.status, JobStatus::Running);
    }

    #[test]
    fn test_claim_no_pending_jobs() {
        let store = test_store();
        let result = store.claim_next_job("worker-1").unwrap();
        assert!(result.is_none());
    }

    // --- Plans ---

    #[test]
    fn test_save_and_get_plans() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut plan = voom_domain::plan::Plan::new(file.clone(), "default", "normalize");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            1,
            voom_domain::plan::ActionParams::Empty,
            "Set default audio",
        )];
        plan.warnings = vec!["test warning".into()];
        plan.policy_hash = Some("abc123".into());

        let plan_id = store.save_plan(&plan).unwrap();
        assert_eq!(plan_id, plan.id);
        let plans = store.plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, plan_id);
        assert_eq!(plans[0].policy_name, "default");
        assert_eq!(plans[0].status, PlanStatus::Pending);
        assert_eq!(plans[0].policy_hash.as_deref(), Some("abc123"));
    }

    // --- Transition processing stats ---

    #[test]
    fn test_transition_processing_stats_roundtrip() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let plan_id = Uuid::new_v4();
        let t = FileTransition::new(
            file.id,
            file.path.clone(),
            "newhash".into(),
            900_000,
            TransitionSource::Voom,
        )
        .with_from(Some("oldhash".into()), Some(1_000_000))
        .with_plan_id(plan_id)
        .with_processing(
            1234,
            3,
            2,
            ProcessingOutcome::Success,
            "default",
            "normalize",
        );

        store.record_transition(&t).unwrap();

        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.duration_ms, Some(1234));
        assert_eq!(r.actions_taken, Some(3));
        assert_eq!(r.tracks_modified, Some(2));
        assert_eq!(r.outcome, Some(ProcessingOutcome::Success));
        assert_eq!(r.policy_name.as_deref(), Some("default"));
        assert_eq!(r.phase_name.as_deref(), Some("normalize"));
        assert_eq!(r.from_size, Some(1_000_000));
        assert_eq!(r.to_size, 900_000);
    }

    #[test]
    fn test_transition_metadata_snapshot_roundtrip() {
        use voom_domain::snapshot::MetadataSnapshot;

        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let snap_file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7200.5)
            .with_tracks(vec![
                Track::new(0, TrackType::Video, "hevc".into()),
                Track::new(1, TrackType::AudioMain, "truehd".into()),
            ]);
        let snap = MetadataSnapshot::from_media_file(&snap_file);

        let t = FileTransition::new(
            file.id,
            PathBuf::from("/movies/test.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Discovery,
        )
        .with_metadata_snapshot(snap.clone());

        store.record_transition(&t).unwrap();

        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_transition_without_metadata_snapshot() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let t = FileTransition::new(
            file.id,
            PathBuf::from("/movies/test2.mkv"),
            "newhash".into(),
            2000,
            TransitionSource::Discovery,
        );

        store.record_transition(&t).unwrap();

        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metadata_snapshot, None);
    }

    #[test]
    fn test_transition_metadata_snapshot_full_roundtrip() {
        use voom_domain::snapshot::MetadataSnapshot;

        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        // Discovery transition without snapshot (typical for new files)
        let t1 = FileTransition::new(
            file.id,
            file.path.clone(),
            "hash1".into(),
            1000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t1).unwrap();

        // Processing transition with snapshot
        let media = MediaFile::new(PathBuf::from("/media/movies/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(7200.5)
            .with_tracks(vec![
                Track::new(0, TrackType::Video, "hevc".into()),
                Track::new(1, TrackType::AudioMain, "aac".into()),
            ]);
        let snap = MetadataSnapshot::from_media_file(&media);

        let t2 = FileTransition::new(
            file.id,
            file.path.clone(),
            "hash2".into(),
            2000,
            TransitionSource::Voom,
        )
        .with_from(Some("hash1".into()), Some(1000))
        .with_processing(
            500,
            2,
            1,
            ProcessingOutcome::Success,
            "default",
            "normalize",
        )
        .with_metadata_snapshot(snap.clone());

        store.record_transition(&t2).unwrap();

        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].metadata_snapshot, None);
        assert_eq!(rows[1].metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_transition_snapshot_serialization_succeeds() {
        use voom_domain::snapshot::MetadataSnapshot;

        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let snap: MetadataSnapshot = serde_json::from_value(serde_json::json!({
            "container": "mkv",
            "video_tracks": 1,
            "audio_tracks": 1,
            "subtitle_tracks": 0,
            "codecs": ["hevc", "aac"],
            "resolution": "1920x1080",
            "duration_secs": 120.5,
        }))
        .expect("valid JSON");

        let t = FileTransition::new(
            file.id,
            PathBuf::from("/media/movies/test.mkv"),
            "hash1".into(),
            1000,
            TransitionSource::Discovery,
        )
        .with_metadata_snapshot(snap.clone());

        store.record_transition(&t).unwrap();

        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metadata_snapshot, Some(snap));
    }

    #[test]
    fn test_prune_missing_files_under_cleans_dependents() {
        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/movies/dep.mkv"));
        file.content_hash = Some("dep".to_string());
        store.upsert_file(&file).unwrap();

        // Save a plan referencing this file
        let mut plan = voom_domain::plan::Plan::new(file.clone(), "test", "normalize");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            0,
            voom_domain::plan::ActionParams::Empty,
            "set default",
        )];
        let _plan_id = store.save_plan(&plan).unwrap();

        // Record a transition with processing stats
        let t = FileTransition::new(
            file.id,
            file.path.clone(),
            "newhash".into(),
            900,
            TransitionSource::Voom,
        )
        .with_from(Some("dep".into()), Some(1000))
        .with_plan_id(plan.id)
        .with_processing(1000, 1, 1, ProcessingOutcome::Success, "test", "normalize");

        store.record_transition(&t).unwrap();

        // Prune — file is missing from disk
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 1);

        // File should be marked as missing (soft-delete)
        let file_result = store.file(&file.id).unwrap();
        assert!(file_result.is_some());
        assert_eq!(
            file_result.unwrap().status,
            voom_domain::FileStatus::Missing
        );

        // Plans should still exist (cleaned up only during purge_missing)
        assert!(!store.plans_for_file(&file.id).unwrap().is_empty());

        // Purge the missing file and its dependents
        let now = chrono::Utc::now();
        let purged = store.purge_missing(now).unwrap();
        assert_eq!(purged, 1);

        // Now file, plans should all be gone
        assert!(store.file(&file.id).unwrap().is_none());
        assert!(store.plans_for_file(&file.id).unwrap().is_empty());
    }

    // --- Plugin data ---

    #[test]
    fn test_plugin_data_set_and_get() {
        let store = test_store();
        store
            .set_plugin_data("ffprobe", "version", b"6.1.0")
            .unwrap();

        let data = store.plugin_data("ffprobe", "version").unwrap();
        assert_eq!(data, Some(b"6.1.0".to_vec()));
    }

    #[test]
    fn test_plugin_data_upsert() {
        let store = test_store();
        store
            .set_plugin_data("ffprobe", "version", b"6.0.0")
            .unwrap();
        store
            .set_plugin_data("ffprobe", "version", b"6.1.0")
            .unwrap();

        let data = store.plugin_data("ffprobe", "version").unwrap();
        assert_eq!(data, Some(b"6.1.0".to_vec()));
    }

    #[test]
    fn test_plugin_data_missing() {
        let store = test_store();
        let data = store.plugin_data("unknown", "key").unwrap();
        assert!(data.is_none());
    }

    // --- Maintenance ---

    #[test]
    fn test_vacuum() {
        let store = test_store();
        // Should not error on empty db
        store.vacuum().unwrap();
    }

    #[test]
    fn test_prune_missing_files() {
        let store = test_store();
        // Insert a file with a path that doesn't exist on disk
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let pruned = store.prune_missing_files().unwrap();
        assert_eq!(pruned, 1);

        // File should be marked as missing (soft-delete)
        let result = store.file(&file.id).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().status, voom_domain::FileStatus::Missing);

        // Now purge the missing file (after retention period)
        let now = chrono::Utc::now();
        let purged = store.purge_missing(now).unwrap();
        assert_eq!(purged, 1);

        // Now the file should be gone
        assert!(store.file(&file.id).unwrap().is_none());
    }

    #[test]
    fn test_prune_missing_files_under_scoped() {
        let store = test_store();

        // Insert files under two different roots
        let mut file_a = MediaFile::new(PathBuf::from("/media/movies/a.mkv"));
        file_a.content_hash = Some("aaa".to_string());
        store.upsert_file(&file_a).unwrap();

        let mut file_b = MediaFile::new(PathBuf::from("/media/tv/b.mkv"));
        file_b.content_hash = Some("bbb".to_string());
        store.upsert_file(&file_b).unwrap();

        // Prune only under /media/movies — both are missing from disk
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 1);

        // file_a should be marked as missing, file_b should remain active
        let file_a_result = store.file(&file_a.id).unwrap();
        assert!(file_a_result.is_some());
        assert_eq!(
            file_a_result.unwrap().status,
            voom_domain::FileStatus::Missing
        );
        let file_b_result = store.file(&file_b.id).unwrap();
        assert!(file_b_result.is_some());
        assert_eq!(
            file_b_result.unwrap().status,
            voom_domain::FileStatus::Active
        );

        // Purge file_a after retention period
        let now = chrono::Utc::now();
        let purged = store.purge_missing(now).unwrap();
        assert_eq!(purged, 1);

        // Now file_a should be gone, file_b should still exist
        assert!(store.file(&file_a.id).unwrap().is_none());
        assert!(store.file(&file_b.id).unwrap().is_some());
    }

    #[test]
    fn test_prune_missing_files_under_different_root_unaffected() {
        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/tv/show.mkv"));
        file.content_hash = Some("show".to_string());
        store.upsert_file(&file).unwrap();

        // Prune under /media/movies — should not touch /media/tv
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 0);

        assert!(store.file(&file.id).unwrap().is_some());
    }

    // --- Concurrency ---

    #[test]
    fn test_concurrent_pool_access() {
        // Use a temp file DB for realistic WAL-mode concurrency (in-memory shared
        // cache doesn't support concurrent transactions well)
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("concurrent_test.db");
        let store = std::sync::Arc::new(SqliteStore::open(&db_path).unwrap());
        let mut handles = vec![];

        for i in 0..4 {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let mut file = MediaFile::new(PathBuf::from(format!("/media/concurrent{i}.mkv")));
                file.content_hash = Some(format!("hash{i}"));
                store.upsert_file(&file).unwrap();
                let loaded = store.file(&file.id).unwrap().unwrap();
                assert_eq!(loaded.path, file.path);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let files = store.list_files(&FileFilters::default()).unwrap();
        assert_eq!(files.len(), 4);
    }

    // --- LIMIT/OFFSET clamping ---

    #[test]
    fn test_list_files_limit_clamped() {
        let store = test_store();
        for i in 0..5 {
            let mut file = MediaFile::new(PathBuf::from(format!("/media/clamp{i}.mkv")));
            file.content_hash = Some(format!("hash_clamp{i}"));
            store.upsert_file(&file).unwrap();
        }

        // Requesting limit > 10_000 should be clamped and still work
        let filters = {
            let mut f = FileFilters::default();
            f.limit = Some(20_000);
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 5); // all 5 returned (clamped to 10_000 which is > 5)
    }

    #[test]
    fn test_list_files_parameterized_limit_offset() {
        let store = test_store();
        for i in 0..10 {
            let mut file = MediaFile::new(PathBuf::from(format!("/media/param{i:02}.mkv")));
            file.content_hash = Some(format!("hash_param{i}"));
            store.upsert_file(&file).unwrap();
        }

        let filters = {
            let mut f = FileFilters::default();
            f.limit = Some(3);
            f.offset = Some(2);
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 3);
        // Files are ordered by path, so offset=2 skips first two
        assert_eq!(files[0].path, PathBuf::from("/media/param02.mkv"));
    }

    #[test]
    fn test_list_jobs_limit_clamped() {
        let store = test_store();
        for i in 0..3 {
            let job = Job::new(voom_domain::job::JobType::Custom(format!("clamp_task{i}")));
            store.create_job(&job).unwrap();
        }

        // Requesting limit > 10_000 should be clamped and still work
        let jobs = store
            .list_jobs(&{
                let mut f = JobFilters::default();
                f.status = None;
                f.limit = Some(20_000);
                f
            })
            .unwrap();
        assert_eq!(jobs.len(), 3);
    }

    // --- update_plan_status ---

    #[test]
    fn test_update_plan_status_completed() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut plan = voom_domain::plan::Plan::new(file.clone(), "default", "normalize");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::SetDefault,
            1,
            voom_domain::plan::ActionParams::Empty,
            "Set default audio",
        )];
        let plan_id = store.save_plan(&plan).unwrap();

        store
            .update_plan_status(&plan_id, PlanStatus::Completed)
            .unwrap();

        let plans = store.plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, PlanStatus::Completed);
        assert!(plans[0].executed_at.is_some());
    }

    #[test]
    fn test_update_plan_status_failed() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut plan = voom_domain::plan::Plan::new(file.clone(), "default", "transcode");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            voom_domain::plan::ActionParams::Transcode {
                codec: "hevc".into(),
                settings: Default::default(),
            },
            "Transcode video",
        )];
        let plan_id = store.save_plan(&plan).unwrap();

        store
            .update_plan_status(&plan_id, PlanStatus::Failed)
            .unwrap();

        let plans = store.plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, PlanStatus::Failed);
    }

    // --- File ID preservation (F1) ---

    #[test]
    fn test_upsert_preserves_id_on_rescan() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/media/preserve_id.mkv"));
        file.content_hash = Some("hash_v1".into());
        store.upsert_file(&file).unwrap();

        let original_id = store
            .file_by_path(Path::new("/media/preserve_id.mkv"))
            .unwrap()
            .unwrap()
            .id;

        // Re-scan creates a new MediaFile with different UUID
        let mut file2 = MediaFile::new(PathBuf::from("/media/preserve_id.mkv"));
        file2.content_hash = Some("hash_v2".into());
        assert_ne!(file2.id, original_id);

        store.upsert_file(&file2).unwrap();

        // The stored file should retain the original ID
        let stored = store
            .file_by_path(Path::new("/media/preserve_id.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, original_id);
        assert_eq!(stored.content_hash, Some("hash_v2".to_string()));
    }

    // --- claim_job_by_id ---

    #[test]
    fn test_claim_job_by_id_pending() {
        let store = test_store();
        let job = Job::new(voom_domain::job::JobType::Custom("test-task".into()));
        let id = store.create_job(&job).unwrap();

        let claimed = store.claim_job_by_id(&id, "worker-1").unwrap();
        assert!(claimed.is_some());
        let claimed = claimed.unwrap();
        assert_eq!(claimed.status, JobStatus::Running);
        assert_eq!(claimed.worker_id.as_deref(), Some("worker-1"));
        assert!(claimed.started_at.is_some());
    }

    #[test]
    fn test_claim_job_by_id_non_pending_returns_none() {
        let store = test_store();
        let job = Job::new(voom_domain::job::JobType::Custom("test-task".into()));
        let id = store.create_job(&job).unwrap();

        // Claim it first
        store.claim_job_by_id(&id, "worker-1").unwrap();

        // Try to claim it again — should return None (already Running)
        let result = store.claim_job_by_id(&id, "worker-2").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_claim_job_by_id_nonexistent_returns_none() {
        let store = test_store();
        let fake_id = Uuid::new_v4();
        let result = store.claim_job_by_id(&fake_id, "worker-1").unwrap();
        assert!(result.is_none());
    }

    // --- prune LIKE escaping ---

    // --- Bad files ---

    fn sample_bad_file() -> BadFile {
        BadFile::new(
            PathBuf::from("/media/movies/corrupt.mkv"),
            1024,
            Some("hash123".into()),
            "ffprobe returned exit code 1".into(),
            BadFileSource::Introspection,
        )
    }

    #[test]
    fn test_upsert_and_get_bad_file() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        let loaded = store
            .bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.path, bf.path);
        assert_eq!(loaded.error, bf.error);
        assert_eq!(loaded.error_source, BadFileSource::Introspection);
        assert_eq!(loaded.attempt_count, 1);
    }

    #[test]
    fn test_upsert_bad_file_increments_attempt_count() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        // Upsert again with different error
        let bf2 = BadFile::new(
            PathBuf::from("/media/movies/corrupt.mkv"),
            1024,
            Some("hash123".into()),
            "new error message".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&bf2).unwrap();

        let loaded = store
            .bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.attempt_count, 2);
        assert_eq!(loaded.error, "new error message");
    }

    #[test]
    fn test_get_bad_file_by_path_not_found() {
        let store = test_store();
        let result = store
            .bad_file_by_path(Path::new("/nonexistent.mkv"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_list_bad_files_no_filter() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        let files = store.list_bad_files(&BadFileFilters::default()).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_list_bad_files_with_path_prefix() {
        let store = test_store();
        let bf1 = sample_bad_file();
        store.upsert_bad_file(&bf1).unwrap();

        let bf2 = BadFile::new(
            PathBuf::from("/other/bad.avi"),
            512,
            None,
            "io error".into(),
            BadFileSource::Io,
        );
        store.upsert_bad_file(&bf2).unwrap();

        let filters = {
            let mut f = BadFileFilters::default();
            f.path_prefix = Some("/media".into());
            f
        };
        let files = store.list_bad_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("/media/movies/corrupt.mkv"));
    }

    #[test]
    fn test_list_bad_files_with_error_source_filter() {
        let store = test_store();
        let bf1 = sample_bad_file();
        store.upsert_bad_file(&bf1).unwrap();

        let bf2 = BadFile::new(
            PathBuf::from("/media/io_error.mkv"),
            512,
            None,
            "io error".into(),
            BadFileSource::Io,
        );
        store.upsert_bad_file(&bf2).unwrap();

        let mut filters = BadFileFilters::default();
        filters.error_source = Some(BadFileSource::Io);
        let files = store.list_bad_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].error_source, BadFileSource::Io);
    }

    #[test]
    fn test_count_bad_files() {
        let store = test_store();
        let no_filter = BadFileFilters::default();
        assert_eq!(store.count_bad_files(&no_filter).unwrap(), 0);

        store.upsert_bad_file(&sample_bad_file()).unwrap();
        assert_eq!(store.count_bad_files(&no_filter).unwrap(), 1);
    }

    #[test]
    fn test_delete_bad_file() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        store.delete_bad_file(&bf.id).unwrap();
        assert!(store.bad_file_by_path(&bf.path).unwrap().is_none());
    }

    #[test]
    fn test_upsert_bad_file_preserves_original_id() {
        let store = test_store();
        let bf1 = sample_bad_file();
        let original_id = bf1.id;
        store.upsert_bad_file(&bf1).unwrap();

        // Upsert same path with a different UUID
        let bf2 = BadFile::new(
            PathBuf::from("/media/movies/corrupt.mkv"),
            1024,
            Some("hash123".into()),
            "different error".into(),
            BadFileSource::Introspection,
        );
        assert_ne!(bf2.id, original_id);
        store.upsert_bad_file(&bf2).unwrap();

        // The original ID should be preserved
        let loaded = store
            .bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id, original_id);
    }

    #[test]
    fn test_delete_bad_file_by_path() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        store.delete_bad_file_by_path(&bf.path).unwrap();
        assert!(store.bad_file_by_path(&bf.path).unwrap().is_none());
    }

    #[test]
    fn test_list_files_like_escaping() {
        let store = test_store();

        // Insert files with LIKE wildcard characters in path
        let mut file1 = MediaFile::new(PathBuf::from("/media/50%_done/video.mkv"));
        file1.content_hash = Some("h1".into());
        store.upsert_file(&file1).unwrap();

        let mut file2 = MediaFile::new(PathBuf::from("/media/50X_done/other.mkv"));
        file2.content_hash = Some("h2".into());
        store.upsert_file(&file2).unwrap();

        let mut file3 = MediaFile::new(PathBuf::from("/media/my_dir/video.mkv"));
        file3.content_hash = Some("h3".into());
        store.upsert_file(&file3).unwrap();

        // path_prefix with % in it should only match literal %
        let filters = {
            let mut f = FileFilters::default();
            f.path_prefix = Some("/media/50%".into());
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("/media/50%_done/video.mkv"));

        // path_prefix with _ should only match literal _
        let filters = {
            let mut f = FileFilters::default();
            f.path_prefix = Some("/media/my_".into());
            f
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("/media/my_dir/video.mkv"));
    }

    #[test]
    fn test_list_bad_files_like_escaping() {
        let store = test_store();

        let bf1 = BadFile::new(
            PathBuf::from("/media/50%_done/corrupt.mkv"),
            1024,
            None,
            "error".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&bf1).unwrap();

        let bf2 = BadFile::new(
            PathBuf::from("/media/50X_done/other.mkv"),
            512,
            None,
            "error".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&bf2).unwrap();

        let filters = {
            let mut f = BadFileFilters::default();
            f.path_prefix = Some("/media/50%".into());
            f
        };
        let files = store.list_bad_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("/media/50%_done/corrupt.mkv"));
    }

    #[test]
    fn test_prune_like_escaping() {
        let store = test_store();

        // Insert a file with an underscore in the path
        let mut file1 = MediaFile::new(PathBuf::from("/media/my_dir/video.mkv"));
        file1.container = Container::Mkv;
        store.upsert_file(&file1).unwrap();

        // Insert a file that would match an unescaped `_` wildcard
        let mut file2 = MediaFile::new(PathBuf::from("/media/myXdir/other.mkv"));
        file2.container = Container::Mkv;
        store.upsert_file(&file2).unwrap();

        // Prune under /media/my_dir/ — should only match file1, not file2
        // Both files exist on disk, so nothing will actually be pruned,
        // but we can verify the query doesn't match file2 by checking counts
        let conn = store.pool.get().unwrap();
        let escaped_root = "/media/my\\_dir/".to_string();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path LIKE ?1 || '%' ESCAPE '\\'",
                params![escaped_root],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "LIKE with escaped underscore should match only exact underscore"
        );
    }

    #[test]
    fn test_sql_query_paginate_limit_and_offset() {
        let mut q = SqlQuery::new("SELECT * FROM files");
        q.paginate(Some(100), Some(50));
        assert!(q.sql.contains("LIMIT"), "expected LIMIT clause");
        assert!(q.sql.contains("OFFSET"), "expected OFFSET clause");
        let refs = q.param_refs();
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_sql_query_paginate_none() {
        let mut q = SqlQuery::new("SELECT * FROM files");
        q.paginate(None, None);
        assert!(!q.sql.contains("LIMIT"), "expected no LIMIT clause");
        assert!(!q.sql.contains("OFFSET"), "expected no OFFSET clause");
        let refs = q.param_refs();
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_sql_query_paginate_clamps_limit() {
        let mut q = SqlQuery::new("SELECT * FROM files");
        q.paginate(Some(99999), None);
        assert!(q.sql.contains("LIMIT"), "expected LIMIT clause");
        assert_eq!(q.params.len(), 1);
        assert_eq!(q.params[0], "10000", "limit should be clamped to 10000");
    }

    // --- Batch reconciliation ---

    use voom_domain::transition::{DiscoveredFile, FileStatus};

    fn discovered(path: &str, size: u64, hash: &str) -> DiscoveredFile {
        DiscoveredFile::new(PathBuf::from(path), size, hash.into())
    }

    #[test]
    fn reconcile_new_file() {
        let store = test_store();
        let files = vec![discovered("/movies/new.mkv", 1000, "hash_new")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.new_files, 1);
        assert_eq!(result.unchanged, 0);
        assert_eq!(result.needs_introspection.len(), 1);
        assert_eq!(
            result.needs_introspection[0],
            PathBuf::from("/movies/new.mkv")
        );

        let file = store
            .file_by_path(Path::new("/movies/new.mkv"))
            .unwrap()
            .expect("file should exist after reconciliation");
        assert_eq!(file.expected_hash.as_deref(), Some("hash_new"));
        assert_eq!(file.content_hash.as_deref(), Some("hash_new"));
        assert_eq!(file.size, 1000);
        assert_eq!(file.status, FileStatus::Active);

        let transitions = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].source, TransitionSource::Discovery);
        assert_eq!(transitions[0].to_hash, "hash_new");
    }

    #[test]
    fn reconcile_unchanged_file() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/unchanged.mkv"));
        file.content_hash = Some("hash_same".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_same").unwrap();

        let files = vec![discovered("/movies/unchanged.mkv", 1000, "hash_same")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.unchanged, 1);
        assert_eq!(result.new_files, 0);
        assert_eq!(result.external_changes, 0);
        assert_eq!(
            result.needs_introspection.len(),
            0,
            "unchanged files need no introspection"
        );

        let transitions = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(transitions.len(), 0, "no transitions for unchanged file");
    }

    #[test]
    fn reconcile_external_modification() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/modified.mkv"));
        file.size = 500;
        file.content_hash = Some("hash_old".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_old").unwrap();
        let original_id = file.id;

        let files = vec![discovered("/movies/modified.mkv", 600, "hash_new")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.external_changes, 1);
        assert_eq!(result.needs_introspection.len(), 1);
        assert_eq!(
            result.needs_introspection[0],
            PathBuf::from("/movies/modified.mkv")
        );

        // Old file should be missing with NULL path
        let old_file = store.file(&original_id).unwrap().expect("old file exists");
        assert_eq!(old_file.status, FileStatus::Missing);

        // New file should exist at the same path with different UUID
        let new_file = store
            .file_by_path(Path::new("/movies/modified.mkv"))
            .unwrap()
            .expect("new file exists");
        assert_ne!(new_file.id, original_id);
        assert_eq!(new_file.expected_hash.as_deref(), Some("hash_new"));

        // Both should have transitions
        let old_transitions = store.transitions_for_file(&original_id).unwrap();
        assert_eq!(old_transitions.len(), 1);
        assert_eq!(old_transitions[0].source, TransitionSource::External);

        let new_transitions = store.transitions_for_file(&new_file.id).unwrap();
        assert_eq!(new_transitions.len(), 1);
        assert_eq!(new_transitions[0].source, TransitionSource::Discovery);
    }

    #[test]
    fn reconcile_missing_file() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/gone.mkv"));
        file.content_hash = Some("hash_gone".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_gone").unwrap();

        let result = store
            .reconcile_discovered_files(&[], &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.missing, 1);

        let loaded = store.file(&file.id).unwrap().expect("file still in DB");
        assert_eq!(loaded.status, FileStatus::Missing);
    }

    #[test]
    fn reconcile_move_detected() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/old_name.mkv"));
        file.content_hash = Some("hash_moved".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_moved").unwrap();
        let original_id = file.id;

        // Mark missing first (simulates previous scan not finding it)
        store.mark_missing(&file.id).unwrap();

        let files = vec![discovered("/movies/new_name.mkv", 1000, "hash_moved")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.moved, 1);
        assert_eq!(result.new_files, 0);
        assert_eq!(result.needs_introspection.len(), 1);
        assert_eq!(
            result.needs_introspection[0],
            PathBuf::from("/movies/new_name.mkv")
        );

        // Same UUID, new path
        let loaded = store.file(&original_id).unwrap().expect("file exists");
        assert_eq!(loaded.path, PathBuf::from("/movies/new_name.mkv"));
        assert_eq!(loaded.status, FileStatus::Active);

        let transitions = store.transitions_for_file(&original_id).unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].source, TransitionSource::Discovery);
        assert_eq!(
            transitions[0].source_detail.as_deref(),
            Some("detected_move")
        );
    }

    #[test]
    fn reconcile_scoped_to_scanned_dirs() {
        let store = test_store();
        let mut movie = MediaFile::new(PathBuf::from("/movies/a.mkv"));
        movie.content_hash = Some("hash_a".into());
        store.upsert_file(&movie).unwrap();
        store.update_expected_hash(&movie.id, "hash_a").unwrap();

        let mut tv = MediaFile::new(PathBuf::from("/tv/b.mkv"));
        tv.content_hash = Some("hash_b".into());
        store.upsert_file(&tv).unwrap();
        store.update_expected_hash(&tv.id, "hash_b").unwrap();

        // Scan only /movies/ with nothing found
        let result = store
            .reconcile_discovered_files(&[], &[PathBuf::from("/movies/")])
            .unwrap();

        assert_eq!(result.missing, 1);

        let movie_loaded = store.file(&movie.id).unwrap().expect("movie in DB");
        assert_eq!(movie_loaded.status, FileStatus::Missing);

        let tv_loaded = store.file(&tv.id).unwrap().expect("tv in DB");
        assert_eq!(tv_loaded.status, FileStatus::Active);
    }

    #[test]
    fn reconcile_reappeared_file() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/back.mkv"));
        file.content_hash = Some("hash_back".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_back").unwrap();
        let original_id = file.id;

        store.mark_missing(&file.id).unwrap();

        // Rediscover at same path with same hash
        let files = vec![discovered("/movies/back.mkv", 1000, "hash_back")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        // Should count as moved (same hash found on missing record)
        // or unchanged if path matches — the spec says path not in DB for move.
        // But the file IS in DB at this path (just missing). Let's check:
        // Path exists in DB + hash matches expected_hash → unchanged, reactivate.
        let loaded = store.file(&original_id).unwrap().expect("file exists");
        assert_eq!(loaded.status, FileStatus::Active);
        assert_eq!(loaded.path, PathBuf::from("/movies/back.mkv"));

        // Should be counted as unchanged (path exists, hash matches)
        assert_eq!(result.unchanged, 1);
    }

    #[test]
    fn reconcile_path_prefix_boundary_no_false_match() {
        // /movies should NOT match /movies2/file.mkv — component-level check required
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies2/file.mkv"));
        file.content_hash = Some("hash_m2".into());
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "hash_m2").unwrap();

        // Scan /movies (no trailing slash) with no discovered files
        let result = store
            .reconcile_discovered_files(&[], &[PathBuf::from("/movies")])
            .unwrap();

        // /movies2/file.mkv is not under /movies — must NOT be marked missing
        assert_eq!(result.missing, 0);
        let loaded = store.file(&file.id).unwrap().expect("file still in DB");
        assert_eq!(loaded.status, FileStatus::Active);
    }

    #[test]
    fn reconcile_null_expected_hash_legacy_file_counts_as_unchanged() {
        // A file with NULL expected_hash re-discovered with any hash should be unchanged
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/movies/legacy.mkv"));
        file.content_hash = Some("old_hash".into());
        file.expected_hash = None; // legacy file — no expected_hash
        store.upsert_file(&file).unwrap();

        let files = vec![discovered("/movies/legacy.mkv", 1000, "new_hash")];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        // NULL expected_hash means we cannot detect external modification;
        // treat as unchanged and backfill expected_hash
        assert_eq!(result.unchanged, 1);
        assert_eq!(result.external_changes, 0);

        let loaded = store
            .file_by_path(Path::new("/movies/legacy.mkv"))
            .unwrap()
            .expect("file exists");
        // expected_hash should now be backfilled with the discovered hash
        assert_eq!(loaded.expected_hash.as_deref(), Some("new_hash"));
        assert_eq!(loaded.status, FileStatus::Active);
    }

    #[test]
    fn reconcile_duplicate_hash_in_discovered_one_move_one_new() {
        // Two discovered files share the same content_hash.
        // One missing file has that hash → exactly one move, one new file.
        let store = test_store();
        let mut missing_file = MediaFile::new(PathBuf::from("/movies/original.mkv"));
        missing_file.content_hash = Some("shared_hash".into());
        store.upsert_file(&missing_file).unwrap();
        store
            .update_expected_hash(&missing_file.id, "shared_hash")
            .unwrap();
        store.mark_missing(&missing_file.id).unwrap();

        let files = vec![
            discovered("/movies/copy_a.mkv", 1000, "shared_hash"),
            discovered("/movies/copy_b.mkv", 1000, "shared_hash"),
        ];
        let result = store
            .reconcile_discovered_files(&files, &[PathBuf::from("/movies/")])
            .unwrap();

        // First match consumes the missing record; second must be a new file
        assert_eq!(result.moved, 1);
        assert_eq!(result.new_files, 1);
        assert_eq!(result.unchanged, 0);
    }

    // --- corrupt metadata_snapshot recovery ---

    #[test]
    fn test_transition_corrupt_metadata_snapshot_returns_none() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let t = FileTransition::new(
            file.id,
            PathBuf::from("/movies/corrupt.mkv"),
            "hash1".into(),
            1000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t).unwrap();

        // Corrupt the snapshot JSON directly via raw SQL
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE file_transitions SET metadata_snapshot = 'not valid json' WHERE file_id = ?1",
                rusqlite::params![file.id.to_string()],
            )
            .unwrap();
        }

        // Reading should succeed — corrupt snapshot becomes None
        let rows = store.transitions_for_file(&file.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].metadata_snapshot, None);
    }

    // --- transitions_for_path ---

    #[test]
    fn transitions_for_path_spans_file_ids() {
        let store = test_store();
        let path = PathBuf::from("/movies/movie.mkv");
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();

        // Insert a file at the path so schema constraints are satisfied
        let mut file = MediaFile::new(path.clone());
        file.id = old_id;
        store.upsert_file(&file).expect("insert old file");

        // Record a transition on the old file
        let t1 = FileTransition::new(
            old_id,
            path.clone(),
            "hash_old".into(),
            1000,
            TransitionSource::Discovery,
        );
        store.record_transition(&t1).expect("record t1");

        // Record a transition on the new file at the same path
        let t2 = FileTransition::new(
            new_id,
            path.clone(),
            "hash_new".into(),
            2000,
            TransitionSource::External,
        );
        store.record_transition(&t2).expect("record t2");

        // Query by path should return both
        let results = store
            .transitions_for_path(&path)
            .expect("transitions_for_path");
        assert_eq!(
            results.len(),
            2,
            "should find transitions from both file IDs"
        );

        let file_ids: std::collections::HashSet<Uuid> = results.iter().map(|t| t.file_id).collect();
        assert!(file_ids.contains(&old_id));
        assert!(file_ids.contains(&new_id));
    }
}
