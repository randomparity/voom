mod bad_file_storage;
mod file_history_storage;
mod file_storage;
mod job_storage;
mod maintenance_storage;
mod plan_storage;
mod plugin_data_storage;
mod stats_storage;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Row};
use uuid::Uuid;

use voom_domain::bad_file::{BadFile, BadFileSource};
use voom_domain::errors::{Result, VoomError};
use voom_domain::job::{Job, JobStatus};
use voom_domain::media::{Container, MediaFile, Track, TrackType};

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
            .map_err(storage_err("failed to create connection pool"))?;

        // Initialize schema on the first connection
        let conn = pool
            .get()
            .map_err(storage_err("failed to get connection"))?;
        schema::create_schema(&conn).map_err(storage_err("failed to create schema"))?;

        Ok(Self { pool })
    }

    pub(crate) fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(storage_err("failed to get connection"))
    }
}

// --- Conversion helpers ---

fn str_to_track_type(s: &str) -> Option<TrackType> {
    match s {
        "video" => Some(TrackType::Video),
        "audio_main" => Some(TrackType::AudioMain),
        "audio_alternate" => Some(TrackType::AudioAlternate),
        "audio_commentary" => Some(TrackType::AudioCommentary),
        "audio_music" => Some(TrackType::AudioMusic),
        "audio_sfx" => Some(TrackType::AudioSfx),
        "audio_non_speech" => Some(TrackType::AudioNonSpeech),
        "subtitle_main" => Some(TrackType::SubtitleMain),
        "subtitle_forced" => Some(TrackType::SubtitleForced),
        "subtitle_commentary" => Some(TrackType::SubtitleCommentary),
        "attachment" => Some(TrackType::Attachment),
        _ => None,
    }
}

/// Create a `.map_err` closure that wraps the source error in `VoomError::Storage`.
pub(crate) fn storage_err<E: std::fmt::Display>(msg: &str) -> impl FnOnce(E) -> VoomError + '_ {
    move |e| VoomError::Storage(format!("{msg}: {e}"))
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
    Uuid::parse_str(s).map_err(|e| VoomError::Storage(format!("invalid UUID '{s}': {e}")))
}

/// Parse a required datetime string, returning a `FromSqlConversionFailure` on corrupt values.
pub(crate) fn parse_required_datetime(s: String, field: &str) -> rusqlite::Result<DateTime<Utc>> {
    s.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("corrupt datetime in {field}: {s}: {e}").into(),
        )
    })
}

/// Parse an optional datetime string, returning an error for corrupt values.
pub(crate) fn parse_optional_datetime(
    s: Option<String>,
    field: &str,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    s.map(|v| {
        v.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("corrupt datetime in {field}: {v}: {e}").into(),
            )
        })
    })
    .transpose()
}

/// Parse an optional JSON string, returning an error for corrupt values.
fn parse_optional_json(
    s: Option<String>,
    field: &str,
) -> rusqlite::Result<Option<serde_json::Value>> {
    s.map(|v| {
        serde_json::from_str(&v).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid JSON in {field}: {e}").into(),
            )
        })
    })
    .transpose()
}

/// Escape LIKE wildcard characters so user-supplied strings match literally.
pub(crate) fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(|e| VoomError::Storage(format!("invalid datetime '{s}': {e}")))
}

pub(crate) fn format_datetime(dt: &DateTime<Utc>) -> String {
    voom_domain::utils::datetime::format_iso(dt)
}

pub(crate) fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: row.get("id")?,
        path: row.get("path")?,
        size: row.get("size")?,
        content_hash: row.get("content_hash")?,
        container: row.get("container")?,
        duration: row.get("duration")?,
        bitrate: row.get("bitrate")?,
        tags: row.get("tags")?,
        plugin_metadata: row.get("plugin_metadata")?,
        introspected_at: row.get("introspected_at")?,
    })
}

pub(crate) struct FileRow {
    pub(crate) id: String,
    path: String,
    size: i64,
    content_hash: String,
    container: String,
    duration: Option<f64>,
    bitrate: Option<i32>,
    tags: Option<String>,
    plugin_metadata: Option<String>,
    introspected_at: String,
}

impl FileRow {
    pub(crate) fn to_media_file(&self, tracks: Vec<Track>) -> Result<MediaFile> {
        let tags: HashMap<String, String> = self
            .tags
            .as_deref()
            .map(|s| {
                serde_json::from_str(s)
                    .map_err(|e| VoomError::Storage(format!("corrupt JSON in files.tags: {e}")))
            })
            .transpose()?
            .unwrap_or_default();

        let plugin_metadata: HashMap<String, serde_json::Value> = self
            .plugin_metadata
            .as_deref()
            .map(|s| {
                serde_json::from_str(s).map_err(|e| {
                    VoomError::Storage(format!("corrupt JSON in files.plugin_metadata: {e}"))
                })
            })
            .transpose()?
            .unwrap_or_default();

        Ok(MediaFile {
            id: parse_uuid(&self.id)?,
            path: PathBuf::from(&self.path),
            size: self.size as u64,
            content_hash: self.content_hash.clone(),
            container: Container::from_extension(&self.container),
            duration: self.duration.unwrap_or(0.0),
            bitrate: self.bitrate.and_then(|b| u32::try_from(b).ok()),
            tracks,
            tags,
            plugin_metadata,
            introspected_at: parse_datetime(&self.introspected_at)?,
        })
    }
}

/// Parse a UUID string from a database row, returning a rusqlite error on corruption.
pub(crate) fn row_uuid(value: &str, table: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("invalid UUID in {table}: {value}: {e}").into(),
        )
    })
}

fn row_to_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    let track_type_str: String = row.get("track_type")?;
    let track_type = str_to_track_type(&track_type_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown track type: {track_type_str}").into(),
        )
    })?;
    Ok(Track {
        index: u32::try_from(row.get::<_, i32>("stream_index")?).unwrap_or(0),
        track_type,
        codec: row.get("codec")?,
        language: row.get("language")?,
        title: row.get("title")?,
        is_default: row.get::<_, i32>("is_default")? != 0,
        is_forced: row.get::<_, i32>("is_forced")? != 0,
        channels: row
            .get::<_, Option<i32>>("channels")?
            .and_then(|v| u32::try_from(v).ok()),
        channel_layout: row.get("channel_layout")?,
        sample_rate: row
            .get::<_, Option<i32>>("sample_rate")?
            .and_then(|v| u32::try_from(v).ok()),
        bit_depth: row
            .get::<_, Option<i32>>("bit_depth")?
            .and_then(|v| u32::try_from(v).ok()),
        width: row
            .get::<_, Option<i32>>("width")?
            .and_then(|v| u32::try_from(v).ok()),
        height: row
            .get::<_, Option<i32>>("height")?
            .and_then(|v| u32::try_from(v).ok()),
        frame_rate: row.get("frame_rate")?,
        is_vfr: row.get::<_, i32>("is_vfr")? != 0,
        is_hdr: row.get::<_, i32>("is_hdr")? != 0,
        hdr_format: row.get("hdr_format")?,
        pixel_format: row.get("pixel_format")?,
    })
}

pub(crate) fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let status_str: String = row.get("status")?;
    let created_str: String = row.get("created_at")?;
    let started_str: Option<String> = row.get("started_at")?;
    let completed_str: Option<String> = row.get("completed_at")?;
    let payload_str: Option<String> = row.get("payload")?;
    let output_str: Option<String> = row.get("output")?;

    let id_str: String = row.get("id")?;
    Ok(Job {
        id: row_uuid(&id_str, "jobs")?,
        job_type: row.get("job_type")?,
        status: JobStatus::parse(&status_str).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown job status: {status_str}").into(),
            )
        })?,
        priority: row.get("priority")?,
        payload: parse_optional_json(payload_str, "jobs.payload")?,
        progress: row.get("progress")?,
        progress_message: row.get("progress_message")?,
        output: parse_optional_json(output_str, "jobs.output")?,
        error: row.get("error")?,
        worker_id: row.get("worker_id")?,
        created_at: parse_required_datetime(created_str, "jobs.created_at")?,
        started_at: parse_optional_datetime(started_str, "jobs.started_at")?,
        completed_at: parse_optional_datetime(completed_str, "jobs.completed_at")?,
    })
}

pub(crate) fn row_to_bad_file(row: &Row<'_>) -> rusqlite::Result<BadFile> {
    let id_str: String = row.get("id")?;
    let path_str: String = row.get("path")?;
    let error_source_str: String = row.get("error_source")?;
    let first_seen_str: String = row.get("first_seen_at")?;
    let last_seen_str: String = row.get("last_seen_at")?;

    Ok(BadFile {
        id: row_uuid(&id_str, "bad_files")?,
        path: PathBuf::from(path_str),
        size: row.get::<_, i64>("size")? as u64,
        content_hash: row.get("content_hash")?,
        error: row.get("error")?,
        error_source: error_source_str.parse::<BadFileSource>().map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown error_source in bad_files: {error_source_str}").into(),
            )
        })?,
        attempt_count: u32::try_from(row.get::<_, i64>("attempt_count")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                format!("invalid attempt_count in bad_files: {e}").into(),
            )
        })?,
        first_seen_at: parse_required_datetime(first_seen_str, "bad_files.first_seen_at")?,
        last_seen_at: parse_required_datetime(last_seen_str, "bad_files.last_seen_at")?,
    })
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

// Private helper methods
impl SqliteStore {
    /// Delete rows from `table` where `column` matches any of `ids`, in chunks of 500.
    /// Returns the total number of rows deleted.
    pub(crate) fn chunked_delete(&self, table: &str, column: &str, ids: &[&str]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
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
                .map_err(|e| VoomError::Storage(format!("failed to delete from {table}: {e}")))?;
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
                "SELECT file_id, stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format \
                 FROM tracks WHERE file_id IN ({}) ORDER BY file_id, stream_index",
                placeholders.join(",")
            );
            let param_values: Vec<String> = chunk.iter().map(|id| id.to_string()).collect();
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
                "SELECT stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format
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
    use voom_domain::media::{Container, Track, TrackType};
    use voom_domain::plan::{OperationType, PlannedAction};
    use voom_domain::storage::{
        BadFileFilters, BadFileStorage, FileFilters, FileHistoryStorage, FileStorage, JobFilters,
        JobStorage, MaintenanceStorage, PlanStatus, PlanStorage, PluginDataStorage, StatsStorage,
    };

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    fn sample_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/movies/test.mkv"));
        file.size = 1_500_000_000;
        file.content_hash = "abc123def456".to_string();
        file.container = Container::Mkv;
        file.duration = 7200.0;
        file.bitrate = Some(8000);
        file.tracks = vec![
            Track::new(0, TrackType::Video, "hevc".into()),
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

        let loaded = store.get_file(&file.id).unwrap().unwrap();
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

        let loaded = store.get_file(&file.id).unwrap().unwrap();
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
            .get_file_by_path(Path::new("/media/movies/test.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id, file.id);
    }

    #[test]
    fn test_get_nonexistent_file() {
        let store = test_store();
        let result = store.get_file(&Uuid::new_v4()).unwrap();
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

        let loaded = store.get_file(&file.id).unwrap().unwrap();
        assert_eq!(loaded.size, 2_000_000_000);
        assert_eq!(loaded.tracks.len(), 4);
    }

    #[test]
    fn test_delete_file() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();
        store.delete_file(&file.id).unwrap();
        assert!(store.get_file(&file.id).unwrap().is_none());
    }

    #[test]
    fn test_delete_cascades_tracks() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();
        store.delete_file(&file.id).unwrap();

        // Verify tracks are also gone
        let conn = store.conn().unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM tracks WHERE file_id = ?1",
                params![file.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
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
        file2.content_hash = "xyz".into();
        store.upsert_file(&file2).unwrap();

        let filters = FileFilters {
            container: Some(Container::Mkv),
            ..Default::default()
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
        file2.content_hash = "xyz".into();
        store.upsert_file(&file2).unwrap();

        let filters = FileFilters {
            path_prefix: Some("/media".into()),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_list_files_with_codec_filter() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let filters = FileFilters {
            has_codec: Some("hevc".into()),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);

        let filters = FileFilters {
            has_codec: Some("av1".into()),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 0);
    }

    #[test]
    fn test_list_files_limit_offset() {
        let store = test_store();
        for i in 0..5 {
            let mut file = MediaFile::new(PathBuf::from(format!("/media/file{i}.mkv")));
            file.content_hash = format!("hash{i}");
            store.upsert_file(&file).unwrap();
        }

        let filters = FileFilters {
            limit: Some(2),
            offset: Some(1),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 2);
    }

    // --- Job CRUD ---

    #[test]
    fn test_create_and_get_job() {
        let store = test_store();
        let mut job = Job::new("transcode".into());
        job.priority = 50;
        job.payload = Some(serde_json::json!({"file": "/test.mkv"}));

        let id = store.create_job(&job).unwrap();
        assert_eq!(id, job.id);

        let loaded = store.get_job(&id).unwrap().unwrap();
        assert_eq!(loaded.job_type, "transcode");
        assert_eq!(loaded.priority, 50);
        assert_eq!(loaded.status, JobStatus::Pending);
    }

    #[test]
    fn test_update_job() {
        let store = test_store();
        let job = Job::new("scan".into());
        store.create_job(&job).unwrap();

        let update = voom_domain::job::JobUpdate {
            status: Some(JobStatus::Running),
            progress: Some(0.5),
            progress_message: Some(Some("Scanning...".into())),
            worker_id: Some(Some("worker-1".into())),
            started_at: Some(Some(Utc::now())),
            ..Default::default()
        };
        store.update_job(&job.id, &update).unwrap();

        let loaded = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(loaded.status, JobStatus::Running);
        assert_eq!(loaded.progress, 0.5);
        assert_eq!(loaded.progress_message.as_deref(), Some("Scanning..."));
        assert_eq!(loaded.worker_id.as_deref(), Some("worker-1"));
        assert!(loaded.started_at.is_some());
    }

    #[test]
    fn test_claim_next_job() {
        let store = test_store();

        let mut job1 = Job::new("task1".into());
        job1.priority = 200;
        store.create_job(&job1).unwrap();

        let mut job2 = Job::new("task2".into());
        job2.priority = 50; // higher priority (lower number)
        store.create_job(&job2).unwrap();

        let claimed = store.claim_next_job("worker-1").unwrap().unwrap();
        assert_eq!(claimed.job_type, "task2"); // lower priority number = claimed first
        assert_eq!(claimed.status, JobStatus::Running);
        assert_eq!(claimed.worker_id.as_deref(), Some("worker-1"));
    }

    #[test]
    fn test_list_jobs() {
        let store = test_store();
        let mut job1 = Job::new("task1".into());
        job1.priority = 100;
        store.create_job(&job1).unwrap();

        let mut job2 = Job::new("task2".into());
        job2.priority = 50;
        store.create_job(&job2).unwrap();

        // Claim one to make it running
        store.claim_next_job("w-1").unwrap();

        let all = store.list_jobs(&JobFilters::default()).unwrap();
        assert_eq!(all.len(), 2);

        let pending = store
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Pending),
                limit: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pending.len(), 1);

        let running = store
            .list_jobs(&JobFilters {
                status: Some(JobStatus::Running),
                limit: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(running.len(), 1);

        let limited = store
            .list_jobs(&JobFilters {
                status: None,
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_count_jobs_by_status() {
        let store = test_store();
        for i in 0..3 {
            let job = Job::new(format!("task{i}"));
            store.create_job(&job).unwrap();
        }
        store.claim_next_job("w-1").unwrap();

        let counts = store.count_jobs_by_status().unwrap();
        let pending = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Pending)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let running = counts
            .iter()
            .find(|(s, _)| *s == JobStatus::Running)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(pending, 2);
        assert_eq!(running, 1);
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

        let plan = voom_domain::plan::Plan {
            id: Uuid::new_v4(),
            file: file.clone(),
            policy_name: "default".into(),
            phase_name: "normalize".into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set default audio".into(),
            }],
            warnings: vec!["test warning".into()],
            skip_reason: None,
            policy_hash: Some("abc123".into()),
            evaluated_at: Utc::now(),
        };

        let plan_id = store.save_plan(&plan).unwrap();
        assert_eq!(plan_id, plan.id);
        let plans = store.get_plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].id, plan_id);
        assert_eq!(plans[0].policy_name, "default");
        assert_eq!(plans[0].status, PlanStatus::Pending);
        assert_eq!(plans[0].policy_hash.as_deref(), Some("abc123"));
    }

    // --- Stats ---

    #[test]
    fn test_record_stats() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut stats =
            voom_domain::stats::ProcessingStats::new(file.id, "default".into(), "normalize".into());
        stats.outcome = voom_domain::stats::ProcessingOutcome::Success;
        stats.duration_ms = 1500;
        stats.actions_taken = 3;
        stats.tracks_modified = 2;
        stats.file_size_before = Some(1_500_000_000);
        stats.file_size_after = Some(1_400_000_000);

        store.record_stats(&stats).unwrap();

        // Verify via direct query
        let conn = store.conn().unwrap();
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM processing_stats WHERE file_id = ?1",
                params![file.id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // --- Plugin data ---

    #[test]
    fn test_plugin_data_set_and_get() {
        let store = test_store();
        store
            .set_plugin_data("ffprobe", "version", b"6.1.0")
            .unwrap();

        let data = store.get_plugin_data("ffprobe", "version").unwrap();
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

        let data = store.get_plugin_data("ffprobe", "version").unwrap();
        assert_eq!(data, Some(b"6.1.0".to_vec()));
    }

    #[test]
    fn test_plugin_data_missing() {
        let store = test_store();
        let data = store.get_plugin_data("unknown", "key").unwrap();
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

        // File should be gone
        assert!(store.get_file(&file.id).unwrap().is_none());
    }

    #[test]
    fn test_prune_missing_files_under_scoped() {
        let store = test_store();

        // Insert files under two different roots
        let mut file_a = MediaFile::new(PathBuf::from("/media/movies/a.mkv"));
        file_a.content_hash = "aaa".to_string();
        store.upsert_file(&file_a).unwrap();

        let mut file_b = MediaFile::new(PathBuf::from("/media/tv/b.mkv"));
        file_b.content_hash = "bbb".to_string();
        store.upsert_file(&file_b).unwrap();

        // Prune only under /media/movies — both are missing from disk
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 1);

        // file_a should be gone, file_b should remain
        assert!(store.get_file(&file_a.id).unwrap().is_none());
        assert!(store.get_file(&file_b.id).unwrap().is_some());
    }

    #[test]
    fn test_prune_missing_files_under_cleans_dependents() {
        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/movies/dep.mkv"));
        file.content_hash = "dep".to_string();
        store.upsert_file(&file).unwrap();

        // Save a plan referencing this file
        let plan = voom_domain::plan::Plan {
            id: uuid::Uuid::new_v4(),
            file: file.clone(),
            policy_name: "test".into(),
            phase_name: "normalize".into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(0),
                parameters: serde_json::json!({}),
                description: "set default".into(),
            }],
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: chrono::Utc::now(),
        };
        let _plan_id = store.save_plan(&plan).unwrap();

        // Record stats referencing this file
        let mut stats =
            voom_domain::stats::ProcessingStats::new(file.id, "test".into(), "normalize".into());
        stats.outcome = voom_domain::stats::ProcessingOutcome::Success;
        stats.duration_ms = 1000;
        stats.actions_taken = 1;
        stats.tracks_modified = 1;
        stats.file_size_before = Some(1000);
        stats.file_size_after = Some(900);
        store.record_stats(&stats).unwrap();

        // Prune — file is missing from disk
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 1);

        // File, plans, and stats should all be gone
        assert!(store.get_file(&file.id).unwrap().is_none());
        assert!(store.get_plans_for_file(&file.id).unwrap().is_empty());
    }

    #[test]
    fn test_prune_missing_files_under_different_root_unaffected() {
        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/tv/show.mkv"));
        file.content_hash = "show".to_string();
        store.upsert_file(&file).unwrap();

        // Prune under /media/movies — should not touch /media/tv
        let pruned = store
            .prune_missing_files_under(Path::new("/media/movies"))
            .unwrap();
        assert_eq!(pruned, 0);

        assert!(store.get_file(&file.id).unwrap().is_some());
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
                file.content_hash = format!("hash{i}");
                store.upsert_file(&file).unwrap();
                let loaded = store.get_file(&file.id).unwrap().unwrap();
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
            file.content_hash = format!("hash_clamp{i}");
            store.upsert_file(&file).unwrap();
        }

        // Requesting limit > 10_000 should be clamped and still work
        let filters = FileFilters {
            limit: Some(20_000),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 5); // all 5 returned (clamped to 10_000 which is > 5)
    }

    #[test]
    fn test_list_files_parameterized_limit_offset() {
        let store = test_store();
        for i in 0..10 {
            let mut file = MediaFile::new(PathBuf::from(format!("/media/param{i:02}.mkv")));
            file.content_hash = format!("hash_param{i}");
            store.upsert_file(&file).unwrap();
        }

        let filters = FileFilters {
            limit: Some(3),
            offset: Some(2),
            ..Default::default()
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
            let job = Job::new(format!("clamp_task{i}"));
            store.create_job(&job).unwrap();
        }

        // Requesting limit > 10_000 should be clamped and still work
        let jobs = store
            .list_jobs(&JobFilters {
                status: None,
                limit: Some(20_000),
                ..Default::default()
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

        let plan = voom_domain::plan::Plan {
            id: Uuid::new_v4(),
            file: file.clone(),
            policy_name: "default".into(),
            phase_name: "normalize".into(),
            actions: vec![PlannedAction {
                operation: OperationType::SetDefault,
                track_index: Some(1),
                parameters: serde_json::json!({}),
                description: "Set default audio".into(),
            }],
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
        };
        let plan_id = store.save_plan(&plan).unwrap();

        store
            .update_plan_status(&plan_id, PlanStatus::Completed)
            .unwrap();

        let plans = store.get_plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, PlanStatus::Completed);
        assert!(plans[0].executed_at.is_some());
    }

    #[test]
    fn test_update_plan_status_failed() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let plan = voom_domain::plan::Plan {
            id: Uuid::new_v4(),
            file: file.clone(),
            policy_name: "default".into(),
            phase_name: "transcode".into(),
            actions: vec![PlannedAction {
                operation: OperationType::TranscodeVideo,
                track_index: Some(0),
                parameters: serde_json::json!({"codec": "hevc"}),
                description: "Transcode video".into(),
            }],
            warnings: vec![],
            skip_reason: None,
            policy_hash: None,
            evaluated_at: Utc::now(),
        };
        let plan_id = store.save_plan(&plan).unwrap();

        store
            .update_plan_status(&plan_id, PlanStatus::Failed)
            .unwrap();

        let plans = store.get_plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, PlanStatus::Failed);
    }

    // --- File ID preservation (F1) ---

    #[test]
    fn test_upsert_preserves_id_on_rescan() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/media/preserve_id.mkv"));
        file.content_hash = "hash_v1".into();
        store.upsert_file(&file).unwrap();

        let original_id = store
            .get_file_by_path(Path::new("/media/preserve_id.mkv"))
            .unwrap()
            .unwrap()
            .id;

        // Re-scan creates a new MediaFile with different UUID
        let mut file2 = MediaFile::new(PathBuf::from("/media/preserve_id.mkv"));
        file2.content_hash = "hash_v2".into();
        assert_ne!(file2.id, original_id);

        store.upsert_file(&file2).unwrap();

        // The stored file should retain the original ID
        let stored = store
            .get_file_by_path(Path::new("/media/preserve_id.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, original_id);
        assert_eq!(stored.content_hash, "hash_v2");
    }

    #[test]
    fn test_upsert_creates_history_on_update() {
        let store = test_store();
        let mut file = MediaFile::new(PathBuf::from("/media/history_test.mkv"));
        file.content_hash = "hash_v1".into();
        file.container = Container::Mkv;
        store.upsert_file(&file).unwrap();

        // No history yet for first insert
        let history = store
            .get_file_history(Path::new("/media/history_test.mkv"))
            .unwrap();
        assert!(history.is_empty());

        // Update the file
        let mut file2 = MediaFile::new(PathBuf::from("/media/history_test.mkv"));
        file2.content_hash = "hash_v2".into();
        file2.container = Container::Mkv;
        store.upsert_file(&file2).unwrap();

        // Now should have one history entry
        let history = store
            .get_file_history(Path::new("/media/history_test.mkv"))
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content_hash, "hash_v1");
        assert_eq!(history[0].container, Container::Mkv);
    }

    #[test]
    fn test_get_file_history_empty() {
        let store = test_store();
        let history = store
            .get_file_history(Path::new("/nonexistent.mkv"))
            .unwrap();
        assert!(history.is_empty());
    }

    // --- claim_job_by_id ---

    #[test]
    fn test_claim_job_by_id_pending() {
        let store = test_store();
        let job = Job::new("test-task".to_string());
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
        let job = Job::new("test-task".to_string());
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
            .get_bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
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
            .get_bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(loaded.attempt_count, 2);
        assert_eq!(loaded.error, "new error message");
    }

    #[test]
    fn test_get_bad_file_by_path_not_found() {
        let store = test_store();
        let result = store
            .get_bad_file_by_path(Path::new("/nonexistent.mkv"))
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

        let filters = BadFileFilters {
            path_prefix: Some("/media".into()),
            ..Default::default()
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

        let filters = BadFileFilters {
            error_source: Some(BadFileSource::Io),
            ..Default::default()
        };
        let files = store.list_bad_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].error_source, BadFileSource::Io);
    }

    #[test]
    fn test_count_bad_files() {
        let store = test_store();
        assert_eq!(store.count_bad_files().unwrap(), 0);

        store.upsert_bad_file(&sample_bad_file()).unwrap();
        assert_eq!(store.count_bad_files().unwrap(), 1);
    }

    #[test]
    fn test_delete_bad_file() {
        let store = test_store();
        let bf = sample_bad_file();
        store.upsert_bad_file(&bf).unwrap();

        store.delete_bad_file(&bf.id).unwrap();
        assert!(store.get_bad_file_by_path(&bf.path).unwrap().is_none());
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
            .get_bad_file_by_path(Path::new("/media/movies/corrupt.mkv"))
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
        assert!(store.get_bad_file_by_path(&bf.path).unwrap().is_none());
    }

    #[test]
    fn test_list_files_like_escaping() {
        let store = test_store();

        // Insert files with LIKE wildcard characters in path
        let mut file1 = MediaFile::new(PathBuf::from("/media/50%_done/video.mkv"));
        file1.content_hash = "h1".into();
        store.upsert_file(&file1).unwrap();

        let mut file2 = MediaFile::new(PathBuf::from("/media/50X_done/other.mkv"));
        file2.content_hash = "h2".into();
        store.upsert_file(&file2).unwrap();

        let mut file3 = MediaFile::new(PathBuf::from("/media/my_dir/video.mkv"));
        file3.content_hash = "h3".into();
        store.upsert_file(&file3).unwrap();

        // path_prefix with % in it should only match literal %
        let filters = FileFilters {
            path_prefix: Some("/media/50%".into()),
            ..Default::default()
        };
        let files = store.list_files(&filters).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, PathBuf::from("/media/50%_done/video.mkv"));

        // path_prefix with _ should only match literal _
        let filters = FileFilters {
            path_prefix: Some("/media/my_".into()),
            ..Default::default()
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

        let filters = BadFileFilters {
            path_prefix: Some("/media/50%".into()),
            ..Default::default()
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
}
