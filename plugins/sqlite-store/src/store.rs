use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Row};
use uuid::Uuid;

use voom_domain::errors::{Result, VoomError};
use voom_domain::job::{Job, JobStatus, JobUpdate};
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::plan::Plan;
use voom_domain::stats::ProcessingStats;
use voom_domain::storage::{FileFilters, StorageTrait, StoredPlan};

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
            .map_err(|e| VoomError::Storage(format!("failed to create connection pool: {e}")))?;

        // Initialize schema on the first connection
        let conn = pool
            .get()
            .map_err(|e| VoomError::Storage(format!("failed to get connection: {e}")))?;
        schema::create_schema(&conn)
            .map_err(|e| VoomError::Storage(format!("failed to create schema: {e}")))?;

        Ok(Self { pool })
    }

    fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|e| VoomError::Storage(format!("failed to get connection: {e}")))
    }
}

// --- Conversion helpers ---

fn str_to_track_type(s: &str) -> TrackType {
    match s {
        "video" => TrackType::Video,
        "audio_main" => TrackType::AudioMain,
        "audio_alternate" => TrackType::AudioAlternate,
        "audio_commentary" => TrackType::AudioCommentary,
        "audio_music" => TrackType::AudioMusic,
        "audio_sfx" => TrackType::AudioSfx,
        "audio_non_speech" => TrackType::AudioNonSpeech,
        "subtitle_main" => TrackType::SubtitleMain,
        "subtitle_forced" => TrackType::SubtitleForced,
        "subtitle_commentary" => TrackType::SubtitleCommentary,
        "attachment" => TrackType::Attachment,
        other => {
            tracing::warn!(
                track_type = other,
                "Unknown track type in database, defaulting to Video"
            );
            TrackType::Video
        }
    }
}

fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| VoomError::Storage(format!("invalid UUID '{s}': {e}")))
}

fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>()
        .map_err(|e| VoomError::Storage(format!("invalid datetime '{s}': {e}")))
}

fn format_datetime(dt: &DateTime<Utc>) -> String {
    voom_domain::utils::datetime::format_iso(dt)
}

fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRow> {
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

struct FileRow {
    id: String,
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
    fn to_media_file(&self, tracks: Vec<Track>) -> Result<MediaFile> {
        let tags: HashMap<String, String> = self
            .tags
            .as_deref()
            .map(|s| {
                serde_json::from_str(s).unwrap_or_else(|e| {
                    tracing::warn!(field = "tags", error = %e, "JSON parse failed, using empty default");
                    HashMap::new()
                })
            })
            .unwrap_or_default();

        let plugin_metadata: HashMap<String, serde_json::Value> = self
            .plugin_metadata
            .as_deref()
            .map(|s| {
                serde_json::from_str(s).unwrap_or_else(|e| {
                    tracing::warn!(field = "plugin_metadata", error = %e, "JSON parse failed, using empty default");
                    HashMap::new()
                })
            })
            .unwrap_or_default();

        Ok(MediaFile {
            id: parse_uuid(&self.id)?,
            path: PathBuf::from(&self.path),
            size: self.size as u64,
            content_hash: self.content_hash.clone(),
            container: Container::from_extension(&self.container),
            duration: self.duration.unwrap_or(0.0),
            bitrate: self.bitrate.map(|b| b as u32),
            tracks,
            tags,
            plugin_metadata,
            introspected_at: parse_datetime(&self.introspected_at)?,
        })
    }
}

fn row_to_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        index: row.get::<_, i32>("stream_index")? as u32,
        track_type: str_to_track_type(row.get::<_, String>("track_type")?.as_str()),
        codec: row.get("codec")?,
        language: row.get("language")?,
        title: row.get("title")?,
        is_default: row.get::<_, i32>("is_default")? != 0,
        is_forced: row.get::<_, i32>("is_forced")? != 0,
        channels: row.get::<_, Option<i32>>("channels")?.map(|v| v as u32),
        channel_layout: row.get("channel_layout")?,
        sample_rate: row.get::<_, Option<i32>>("sample_rate")?.map(|v| v as u32),
        bit_depth: row.get::<_, Option<i32>>("bit_depth")?.map(|v| v as u32),
        width: row.get::<_, Option<i32>>("width")?.map(|v| v as u32),
        height: row.get::<_, Option<i32>>("height")?.map(|v| v as u32),
        frame_rate: row.get("frame_rate")?,
        is_vfr: row.get::<_, i32>("is_vfr")? != 0,
        is_hdr: row.get::<_, i32>("is_hdr")? != 0,
        hdr_format: row.get("hdr_format")?,
        pixel_format: row.get("pixel_format")?,
    })
}

fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let status_str: String = row.get("status")?;
    let created_str: String = row.get("created_at")?;
    let started_str: Option<String> = row.get("started_at")?;
    let completed_str: Option<String> = row.get("completed_at")?;
    let payload_str: Option<String> = row.get("payload")?;
    let output_str: Option<String> = row.get("output")?;

    Ok(Job {
        id: Uuid::parse_str(&row.get::<_, String>("id")?).unwrap_or_default(),
        job_type: row.get("job_type")?,
        status: JobStatus::parse(&status_str).unwrap_or(JobStatus::Pending),
        priority: row.get("priority")?,
        payload: payload_str.and_then(|s| serde_json::from_str(&s).ok()),
        progress: row.get("progress")?,
        progress_message: row.get("progress_message")?,
        output: output_str.and_then(|s| serde_json::from_str(&s).ok()),
        error: row.get("error")?,
        worker_id: row.get("worker_id")?,
        created_at: created_str.parse().unwrap_or_else(|_| Utc::now()),
        started_at: started_str.and_then(|s| s.parse().ok()),
        completed_at: completed_str.and_then(|s| s.parse().ok()),
    })
}

impl StorageTrait for SqliteStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let tags_json = serde_json::to_string(&file.tags)
            .map_err(|e| VoomError::Storage(format!("failed to serialize tags: {e}")))?;
        let meta_json = serde_json::to_string(&file.plugin_metadata)
            .map_err(|e| VoomError::Storage(format!("failed to serialize metadata: {e}")))?;
        let filename = file
            .path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        let path_str = file.path.to_string_lossy().to_string();

        // Preserve existing file ID on re-scan to avoid orphaning related records
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![&path_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VoomError::Storage(format!("failed to query existing file: {e}")))?;

        let effective_id = existing_id.clone().unwrap_or_else(|| file.id.to_string());

        // Wrap the delete-insert sequence in a transaction for atomicity
        conn.execute_batch("BEGIN")
            .map_err(|e| VoomError::Storage(format!("failed to begin transaction: {e}")))?;

        let result = (|| -> Result<()> {
            // Archive old file state to history before updating
            if existing_id.is_some() {
                conn.execute(
                    "INSERT INTO file_history (id, file_id, path, content_hash, container, track_count, introspected_at, archived_at)
                     SELECT ?1, f.id, f.path, f.content_hash, f.container,
                            (SELECT COUNT(*) FROM tracks WHERE file_id = f.id),
                            f.introspected_at, ?2
                     FROM files f WHERE f.path = ?3",
                    params![Uuid::new_v4().to_string(), &now, &path_str],
                )
                .map_err(|e| VoomError::Storage(format!("failed to archive file history: {e}")))?;
            }

            // Delete old tracks before upserting
            conn.execute(
                "DELETE FROM tracks WHERE file_id IN (SELECT id FROM files WHERE path = ?1)",
                params![&path_str],
            )
            .map_err(|e| VoomError::Storage(format!("failed to delete old tracks: {e}")))?;

            conn.execute(
                "INSERT INTO files (id, path, filename, size, content_hash, container, duration, bitrate, tags, plugin_metadata, introspected_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(path) DO UPDATE SET
                    filename = excluded.filename,
                    size = excluded.size,
                    content_hash = excluded.content_hash,
                    container = excluded.container,
                    duration = excluded.duration,
                    bitrate = excluded.bitrate,
                    tags = excluded.tags,
                    plugin_metadata = excluded.plugin_metadata,
                    introspected_at = excluded.introspected_at,
                    updated_at = excluded.updated_at",
                params![
                    &effective_id,
                    &path_str,
                    filename,
                    file.size as i64,
                    file.content_hash,
                    file.container.as_str(),
                    file.duration,
                    file.bitrate.map(|b| b as i32),
                    tags_json,
                    meta_json,
                    format_datetime(&file.introspected_at),
                    &now,
                    &now,
                ],
            )
            .map_err(|e| VoomError::Storage(format!("failed to upsert file: {e}")))?;

            let mut stmt = conn
                .prepare(
                    "INSERT INTO tracks (id, file_id, stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                )
                .map_err(|e| VoomError::Storage(format!("failed to prepare track insert: {e}")))?;

            for track in &file.tracks {
                stmt.execute(params![
                    Uuid::new_v4().to_string(),
                    &effective_id,
                    track.index as i32,
                    track.track_type.as_str(),
                    track.codec,
                    track.language,
                    track.title,
                    track.is_default as i32,
                    track.is_forced as i32,
                    track.channels.map(|v| v as i32),
                    track.channel_layout,
                    track.sample_rate.map(|v| v as i32),
                    track.bit_depth.map(|v| v as i32),
                    track.width.map(|v| v as i32),
                    track.height.map(|v| v as i32),
                    track.frame_rate,
                    track.is_vfr as i32,
                    track.is_hdr as i32,
                    track.hdr_format,
                    track.pixel_format,
                ])
                .map_err(|e| VoomError::Storage(format!("failed to insert track: {e}")))?;
            }

            Ok(())
        })();

        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| VoomError::Storage(format!("failed to commit: {e}")))?;
                Ok(())
            }
            Err(e) => {
                if let Err(rollback_err) = conn.execute_batch("ROLLBACK") {
                    tracing::error!(error = %rollback_err, "ROLLBACK failed");
                }
                Err(e)
            }
        }
    }

    fn get_file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE id = ?1",
                params![id.to_string()],
                row_to_file,
            )
            .optional()
            .map_err(|e| VoomError::Storage(format!("failed to get file: {e}")))?;

        match file_row {
            Some(fr) => {
                let tracks = self.load_tracks(&conn, id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
    }

    fn get_file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE path = ?1",
                params![path_str],
                row_to_file,
            )
            .optional()
            .map_err(|e| VoomError::Storage(format!("failed to get file by path: {e}")))?;

        match file_row {
            Some(fr) => {
                let id = parse_uuid(&fr.id)?;
                let tracks = self.load_tracks(&conn, &id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let conn = self.conn()?;
        let mut sql = String::from(
            "SELECT id, path, size, content_hash, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE 1=1",
        );
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref container) = filters.container {
            param_values.push(container.clone());
            sql.push_str(&format!(" AND container = ?{}", param_values.len()));
        }
        if let Some(ref prefix) = filters.path_prefix {
            param_values.push(format!("{prefix}%"));
            sql.push_str(&format!(" AND path LIKE ?{}", param_values.len()));
        }

        sql.push_str(" ORDER BY path");

        if let Some(limit) = filters.limit {
            let clamped = limit.min(10_000);
            param_values.push(clamped.to_string());
            sql.push_str(&format!(" LIMIT ?{}", param_values.len()));
        }
        if let Some(offset) = filters.offset {
            let clamped = offset.min(1_000_000);
            param_values.push(clamped.to_string());
            sql.push_str(&format!(" OFFSET ?{}", param_values.len()));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| VoomError::Storage(format!("failed to prepare list query: {e}")))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let rows: Vec<FileRow> = stmt
            .query_map(param_refs.as_slice(), row_to_file)
            .map_err(|e| VoomError::Storage(format!("failed to list files: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect files: {e}")))?;

        let file_ids: Vec<Uuid> = rows
            .iter()
            .map(|fr| parse_uuid(&fr.id))
            .collect::<Result<Vec<_>>>()?;
        let tracks_map = self.load_tracks_batch(&conn, &file_ids)?;

        let mut files = Vec::with_capacity(rows.len());
        for (fr, id) in rows.iter().zip(file_ids.iter()) {
            let tracks = tracks_map.get(id).cloned().unwrap_or_default();
            files.push(fr.to_media_file(tracks)?);
        }

        // Post-filter for codec/language (requires track data)
        let files = if filters.has_codec.is_some() || filters.has_language.is_some() {
            files
                .into_iter()
                .filter(|f| {
                    if let Some(ref codec) = filters.has_codec {
                        if !f.tracks.iter().any(|t| t.codec == *codec) {
                            return false;
                        }
                    }
                    if let Some(ref lang) = filters.has_language {
                        if !f.tracks.iter().any(|t| t.language == *lang) {
                            return false;
                        }
                    }
                    true
                })
                .collect()
        } else {
            files
        };

        Ok(files)
    }

    fn count_files(&self, filters: &FileFilters) -> Result<u64> {
        let conn = self.conn()?;
        let mut sql = String::from("SELECT COUNT(*) FROM files WHERE 1=1");
        let mut param_values: Vec<String> = Vec::new();

        if let Some(ref container) = filters.container {
            param_values.push(container.clone());
            sql.push_str(&format!(" AND container = ?{}", param_values.len()));
        }
        if let Some(ref prefix) = filters.path_prefix {
            param_values.push(format!("{prefix}%"));
            sql.push_str(&format!(" AND path LIKE ?{}", param_values.len()));
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let count: u64 = conn
            .query_row(&sql, param_refs.as_slice(), |row| row.get(0))
            .map_err(|e| VoomError::Storage(format!("failed to count files: {e}")))?;

        Ok(count)
    }

    fn delete_file(&self, id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        conn.execute("DELETE FROM files WHERE id = ?1", params![id.to_string()])
            .map_err(|e| VoomError::Storage(format!("failed to delete file: {e}")))?;
        Ok(())
    }

    fn create_job(&self, job: &Job) -> Result<Uuid> {
        let conn = self.conn()?;
        let payload_json = job
            .payload
            .as_ref()
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let output_json = job
            .output
            .as_ref()
            .map(|o| serde_json::to_string(o).unwrap_or_default());

        conn.execute(
            "INSERT INTO jobs (id, job_type, status, priority, payload, progress, progress_message, output, error, worker_id, created_at, started_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                job.id.to_string(),
                job.job_type,
                job.status.as_str(),
                job.priority,
                payload_json,
                job.progress,
                job.progress_message,
                output_json,
                job.error,
                job.worker_id,
                format_datetime(&job.created_at),
                job.started_at.as_ref().map(format_datetime),
                job.completed_at.as_ref().map(format_datetime),
            ],
        )
        .map_err(|e| VoomError::Storage(format!("failed to create job: {e}")))?;

        Ok(job.id)
    }

    fn get_job(&self, id: &Uuid) -> Result<Option<Job>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT * FROM jobs WHERE id = ?1",
            params![id.to_string()],
            row_to_job,
        )
        .optional()
        .map_err(|e| VoomError::Storage(format!("failed to get job: {e}")))
    }

    fn update_job(&self, id: &Uuid, update: &JobUpdate) -> Result<()> {
        let conn = self.conn()?;
        let mut sets = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(status) = &update.status {
            param_values.push(Box::new(status.as_str().to_string()));
            sets.push(format!("status = ?{}", param_values.len()));
        }
        if let Some(progress) = &update.progress {
            param_values.push(Box::new(*progress));
            sets.push(format!("progress = ?{}", param_values.len()));
        }
        if let Some(ref msg) = update.progress_message {
            param_values.push(Box::new(msg.clone()));
            sets.push(format!("progress_message = ?{}", param_values.len()));
        }
        if let Some(ref output) = update.output {
            let json = output
                .as_ref()
                .map(|o| serde_json::to_string(o).unwrap_or_default());
            param_values.push(Box::new(json));
            sets.push(format!("output = ?{}", param_values.len()));
        }
        if let Some(ref error) = update.error {
            param_values.push(Box::new(error.clone()));
            sets.push(format!("error = ?{}", param_values.len()));
        }
        if let Some(ref worker) = update.worker_id {
            param_values.push(Box::new(worker.clone()));
            sets.push(format!("worker_id = ?{}", param_values.len()));
        }
        if let Some(ref started) = update.started_at {
            param_values.push(Box::new(started.as_ref().map(format_datetime)));
            sets.push(format!("started_at = ?{}", param_values.len()));
        }
        if let Some(ref completed) = update.completed_at {
            param_values.push(Box::new(completed.as_ref().map(format_datetime)));
            sets.push(format!("completed_at = ?{}", param_values.len()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        param_values.push(Box::new(id.to_string()));
        let sql = format!(
            "UPDATE jobs SET {} WHERE id = ?{}",
            sets.join(", "),
            param_values.len()
        );

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        conn.execute(&sql, param_refs.as_slice())
            .map_err(|e| VoomError::Storage(format!("failed to update job: {e}")))?;
        Ok(())
    }

    fn claim_next_job(&self, worker_id: &str) -> Result<Option<Job>> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());

        // Use IMMEDIATE transaction to prevent TOCTOU race between concurrent workers
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| VoomError::Storage(format!("failed to begin transaction: {e}")))?;

        tx.execute(
            "UPDATE jobs SET status = 'running', worker_id = ?1, started_at = ?2
             WHERE id = (SELECT id FROM jobs WHERE status = 'pending' ORDER BY priority ASC, created_at ASC LIMIT 1)",
            params![worker_id, now],
        )
        .map_err(|e| VoomError::Storage(format!("failed to claim job: {e}")))?;

        let result = tx
            .query_row(
                "SELECT * FROM jobs WHERE worker_id = ?1 AND status = 'running' ORDER BY started_at DESC LIMIT 1",
                params![worker_id],
                row_to_job,
            )
            .optional()
            .map_err(|e| VoomError::Storage(format!("failed to get claimed job: {e}")))?;

        tx.commit()
            .map_err(|e| VoomError::Storage(format!("failed to commit claim: {e}")))?;

        Ok(result)
    }

    fn list_jobs(&self, status: Option<JobStatus>, limit: Option<u32>) -> Result<Vec<Job>> {
        let conn = self.conn()?;
        let mut sql = String::from("SELECT * FROM jobs");
        let mut param_values: Vec<String> = Vec::new();

        if let Some(status) = status {
            param_values.push(status.as_str().to_string());
            sql.push_str(&format!(" WHERE status = ?{}", param_values.len()));
        }

        sql.push_str(" ORDER BY priority ASC, created_at DESC");

        if let Some(limit) = limit {
            let clamped = limit.min(10_000);
            param_values.push(clamped.to_string());
            sql.push_str(&format!(" LIMIT ?{}", param_values.len()));
        }

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| VoomError::Storage(format!("failed to prepare list jobs query: {e}")))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let jobs = stmt
            .query_map(param_refs.as_slice(), row_to_job)
            .map_err(|e| VoomError::Storage(format!("failed to list jobs: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect jobs: {e}")))?;

        Ok(jobs)
    }

    fn count_jobs_by_status(&self) -> Result<Vec<(JobStatus, u64)>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")
            .map_err(|e| VoomError::Storage(format!("failed to prepare count query: {e}")))?;

        let counts = stmt
            .query_map([], |row| {
                let status_str: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((status_str, count as u64))
            })
            .map_err(|e| VoomError::Storage(format!("failed to count jobs: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect counts: {e}")))?;

        let result = counts
            .into_iter()
            .filter_map(|(s, c)| JobStatus::parse(&s).map(|status| (status, c)))
            .collect();

        Ok(result)
    }

    fn save_plan(&self, plan: &Plan) -> Result<Uuid> {
        let conn = self.conn()?;
        let actions_json = serde_json::to_string(&plan.actions)
            .map_err(|e| VoomError::Storage(format!("failed to serialize actions: {e}")))?;
        let warnings_json =
            if plan.warnings.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&plan.warnings).map_err(|e| {
                    VoomError::Storage(format!("failed to serialize warnings: {e}"))
                })?)
            };

        // Resolve file_id by path to handle ID preservation in upsert_file.
        // When a file is re-scanned, upsert_file keeps the original DB ID, but
        // the Plan's file.id may be a fresh UUID from the new introspection.
        let path_str = plan.file.path.to_string_lossy().to_string();
        let effective_file_id: String = conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![&path_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| VoomError::Storage(format!("failed to resolve file id: {e}")))?
            .unwrap_or_else(|| plan.file.id.to_string());

        conn.execute(
            "INSERT INTO plans (id, file_id, policy_name, phase_name, status, actions, warnings, skip_reason, policy_hash, evaluated_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                plan.id.to_string(),
                effective_file_id,
                plan.policy_name,
                plan.phase_name,
                "pending",
                actions_json,
                warnings_json,
                plan.skip_reason,
                plan.policy_hash,
                format_datetime(&plan.evaluated_at),
                format_datetime(&Utc::now()),
            ],
        )
        .map_err(|e| VoomError::Storage(format!("failed to save plan: {e}")))?;

        Ok(plan.id)
    }

    fn update_plan_status(&self, plan_id: &Uuid, status: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE plans SET status = ?1, executed_at = ?2 WHERE id = ?3",
            params![status, format_datetime(&Utc::now()), plan_id.to_string()],
        )
        .map_err(|e| VoomError::Storage(format!("failed to update plan status: {e}")))?;
        Ok(())
    }

    fn get_plans_for_file(&self, file_id: &Uuid) -> Result<Vec<StoredPlan>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, policy_name, phase_name, status, actions, warnings, skip_reason, policy_hash, evaluated_at, created_at, executed_at, result
                 FROM plans WHERE file_id = ?1 ORDER BY created_at",
            )
            .map_err(|e| VoomError::Storage(format!("failed to prepare plans query: {e}")))?;

        let plans = stmt
            .query_map(params![file_id.to_string()], |row| {
                Ok(StoredPlan {
                    id: Uuid::parse_str(&row.get::<_, String>("id")?).unwrap_or_default(),
                    file_id: Uuid::parse_str(&row.get::<_, String>("file_id")?).unwrap_or_default(),
                    policy_name: row.get("policy_name")?,
                    phase_name: row.get("phase_name")?,
                    status: row.get("status")?,
                    actions_json: row.get("actions")?,
                    warnings: row.get("warnings")?,
                    skip_reason: row.get("skip_reason")?,
                    policy_hash: row.get("policy_hash")?,
                    evaluated_at: row.get("evaluated_at")?,
                    created_at: row.get("created_at")?,
                    executed_at: row.get("executed_at")?,
                    result: row.get("result")?,
                })
            })
            .map_err(|e| VoomError::Storage(format!("failed to query plans: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect plans: {e}")))?;

        Ok(plans)
    }

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
                stats.outcome,
                stats.duration_ms as i64,
                stats.actions_taken as i32,
                stats.tracks_modified as i32,
                stats.file_size_before.map(|v| v as i64),
                stats.file_size_after.map(|v| v as i64),
                format_datetime(&stats.created_at),
            ],
        )
        .map_err(|e| VoomError::Storage(format!("failed to record stats: {e}")))?;
        Ok(())
    }

    fn get_plugin_data(&self, plugin: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT value FROM plugin_data WHERE plugin_name = ?1 AND key = ?2",
            params![plugin, key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| VoomError::Storage(format!("failed to get plugin data: {e}")))
    }

    fn set_plugin_data(&self, plugin: &str, key: &str, value: &[u8]) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO plugin_data (plugin_name, key, value, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(plugin_name, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![plugin, key, value, format_datetime(&Utc::now())],
        )
        .map_err(|e| VoomError::Storage(format!("failed to set plugin data: {e}")))?;
        Ok(())
    }

    fn vacuum(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch("VACUUM")
            .map_err(|e| VoomError::Storage(format!("failed to vacuum: {e}")))?;
        Ok(())
    }

    fn prune_missing_files(&self) -> Result<u64> {
        self.prune_missing_files_under(Path::new("/"))
    }

    fn prune_missing_files_under(&self, root: &Path) -> Result<u64> {
        let root_str = root.to_string_lossy().to_string();

        // Phase 1: Query file paths under root (release connection after)
        let files: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT id, path FROM files WHERE path LIKE ?1 || '%'")
                .map_err(|e| VoomError::Storage(format!("failed to prepare prune query: {e}")))?;

            let result = stmt
                .query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| VoomError::Storage(format!("failed to query files: {e}")))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| VoomError::Storage(format!("failed to collect files: {e}")))?;
            result
        };

        // Phase 2: Check filesystem (no connection held)
        let missing_ids: Vec<&str> = files
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .map(|(id, _)| id.as_str())
            .collect();

        if missing_ids.is_empty() {
            return Ok(0);
        }

        // Phase 3: Batch delete dependents then files in chunks of 500.
        // Explicit deletion of plans and processing_stats ensures cleanup works
        // on existing databases where CASCADE constraints may be missing.
        let conn = self.conn()?;
        let mut pruned = 0u64;
        for chunk in missing_ids.chunks(500) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let in_clause = placeholders.join(",");
            let param_refs: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();

            // Delete dependent rows first
            conn.execute(
                &format!("DELETE FROM plans WHERE file_id IN ({in_clause})"),
                param_refs.as_slice(),
            )
            .map_err(|e| VoomError::Storage(format!("failed to delete plans: {e}")))?;

            conn.execute(
                &format!("DELETE FROM processing_stats WHERE file_id IN ({in_clause})"),
                param_refs.as_slice(),
            )
            .map_err(|e| VoomError::Storage(format!("failed to delete processing_stats: {e}")))?;

            let deleted = conn
                .execute(
                    &format!("DELETE FROM files WHERE id IN ({in_clause})"),
                    param_refs.as_slice(),
                )
                .map_err(|e| VoomError::Storage(format!("failed to delete files: {e}")))?;
            pruned += deleted as u64;
        }

        Ok(pruned)
    }

    fn get_file_history(&self, path: &Path) -> Result<Vec<voom_domain::storage::FileHistoryEntry>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, content_hash, container, track_count, introspected_at, archived_at
                 FROM file_history WHERE path = ?1 ORDER BY archived_at",
            )
            .map_err(|e| VoomError::Storage(format!("failed to prepare history query: {e}")))?;

        let entries = stmt
            .query_map(params![path_str], |row| {
                Ok(voom_domain::storage::FileHistoryEntry {
                    id: Uuid::parse_str(&row.get::<_, String>("id")?).unwrap_or_default(),
                    file_id: Uuid::parse_str(&row.get::<_, String>("file_id")?).unwrap_or_default(),
                    path: PathBuf::from(row.get::<_, String>("path")?),
                    content_hash: row.get("content_hash")?,
                    container: row.get("container")?,
                    track_count: row.get::<_, i32>("track_count")? as u32,
                    introspected_at: row.get("introspected_at")?,
                    archived_at: row.get("archived_at")?,
                })
            })
            .map_err(|e| VoomError::Storage(format!("failed to query history: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect history: {e}")))?;

        Ok(entries)
    }
}

// Private helper methods
impl SqliteStore {
    fn load_tracks_batch(
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

            let mut stmt = conn.prepare(&sql).map_err(|e| {
                VoomError::Storage(format!("failed to prepare batch track query: {e}"))
            })?;

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let file_id_str: String = row.get("file_id")?;
                    let track = row_to_track(row)?;
                    Ok((file_id_str, track))
                })
                .map_err(|e| VoomError::Storage(format!("failed to batch query tracks: {e}")))?;

            for row_result in rows {
                let (file_id_str, track) = row_result
                    .map_err(|e| VoomError::Storage(format!("failed to read track row: {e}")))?;
                let file_id = parse_uuid(&file_id_str)?;
                result.entry(file_id).or_default().push(track);
            }
        }

        Ok(result)
    }

    fn load_tracks(&self, conn: &rusqlite::Connection, file_id: &Uuid) -> Result<Vec<Track>> {
        let mut stmt = conn
            .prepare(
                "SELECT stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format
                 FROM tracks WHERE file_id = ?1 ORDER BY stream_index",
            )
            .map_err(|e| VoomError::Storage(format!("failed to prepare track query: {e}")))?;

        let tracks = stmt
            .query_map(params![file_id.to_string()], row_to_track)
            .map_err(|e| VoomError::Storage(format!("failed to query tracks: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| VoomError::Storage(format!("failed to collect tracks: {e}")))?;

        Ok(tracks)
    }
}

/// Extension trait for `rusqlite::Result<T>` to convert to `Option<T>`.
trait OptionalExt<T> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::media::{Container, Track, TrackType};
    use voom_domain::plan::{OperationType, PlannedAction};

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
            container: Some("mkv".into()),
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

        let update = JobUpdate {
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

        let all = store.list_jobs(None, None).unwrap();
        assert_eq!(all.len(), 2);

        let pending = store.list_jobs(Some(JobStatus::Pending), None).unwrap();
        assert_eq!(pending.len(), 1);

        let running = store.list_jobs(Some(JobStatus::Running), None).unwrap();
        assert_eq!(running.len(), 1);

        let limited = store.list_jobs(None, Some(1)).unwrap();
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

        let plan = Plan {
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
        assert_eq!(plans[0].status, "pending");
        assert_eq!(plans[0].policy_hash.as_deref(), Some("abc123"));
    }

    // --- Stats ---

    #[test]
    fn test_record_stats() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let mut stats = ProcessingStats::new(file.id, "default".into(), "normalize".into());
        stats.outcome = "completed".into();
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
        let plan_id = store.save_plan(&plan).unwrap();

        // Record stats referencing this file
        let mut stats =
            voom_domain::stats::ProcessingStats::new(file.id, "test".into(), "normalize".into());
        stats.outcome = "success".into();
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
        let jobs = store.list_jobs(None, Some(20_000)).unwrap();
        assert_eq!(jobs.len(), 3);
    }

    // --- update_plan_status ---

    #[test]
    fn test_update_plan_status_completed() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let plan = Plan {
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

        store.update_plan_status(&plan_id, "completed").unwrap();

        let plans = store.get_plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, "completed");
        assert!(plans[0].executed_at.is_some());
    }

    #[test]
    fn test_update_plan_status_failed() {
        let store = test_store();
        let file = sample_file();
        store.upsert_file(&file).unwrap();

        let plan = Plan {
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

        store.update_plan_status(&plan_id, "failed").unwrap();

        let plans = store.get_plans_for_file(&file.id).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].status, "failed");
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
        assert_eq!(history[0].container, "mkv");
    }

    #[test]
    fn test_get_file_history_empty() {
        let store = test_store();
        let history = store
            .get_file_history(Path::new("/nonexistent.mkv"))
            .unwrap();
        assert!(history.is_empty());
    }
}
