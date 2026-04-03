use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::Row;
use uuid::Uuid;

use voom_domain::bad_file::{BadFile, BadFileSource};
use voom_domain::errors::Result;
use voom_domain::job::{Job, JobStatus};
use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_domain::transition::FileStatus;

use super::{other_storage_err, parse_datetime, parse_uuid};

fn str_to_track_type(s: &str) -> Option<TrackType> {
    s.parse().ok()
}

/// Parse a required datetime string, returning a
/// `FromSqlConversionFailure` on corrupt values.
pub(crate) fn parse_required_datetime(s: String, field: &str) -> rusqlite::Result<DateTime<Utc>> {
    s.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("corrupt datetime in {field}: {s}: {e}").into(),
        )
    })
}

/// Parse an optional datetime string, returning an error
/// for corrupt values.
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

pub(crate) fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: row.get("id")?,
        path: row.get("path")?,
        size: row.get("size")?,
        content_hash: row.get("content_hash")?,
        expected_hash: row.get("expected_hash")?,
        status: row
            .get::<_, Option<String>>("status")?
            .unwrap_or_else(|| "active".to_string()),
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
    expected_hash: Option<String>,
    status: String,
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
                serde_json::from_str(s).map_err(other_storage_err("corrupt JSON in files.tags"))
            })
            .transpose()?
            .unwrap_or_default();

        let plugin_metadata: HashMap<String, serde_json::Value> = self
            .plugin_metadata
            .as_deref()
            .map(|s| {
                serde_json::from_str(s)
                    .map_err(other_storage_err("corrupt JSON in files.plugin_metadata"))
            })
            .transpose()?
            .unwrap_or_default();

        let mut mf = MediaFile::new(PathBuf::from(&self.path));
        mf.id = parse_uuid(&self.id)?;
        mf.size = self.size as u64;
        mf.content_hash = if self.content_hash.is_empty() {
            None
        } else {
            Some(self.content_hash.clone())
        };
        mf.container = Container::from_extension(&self.container);
        mf.duration = self.duration.unwrap_or(0.0);
        mf.bitrate = self.bitrate.and_then(|b| u32::try_from(b).ok());
        mf.tracks = tracks;
        mf.tags = tags;
        mf.plugin_metadata = plugin_metadata;
        mf.introspected_at = parse_datetime(&self.introspected_at)?;
        mf.expected_hash = self.expected_hash.clone();
        mf.status = FileStatus::parse(&self.status).unwrap_or_default();
        Ok(mf)
    }
}

/// Parse a UUID string from a database row, returning a rusqlite
/// error on corruption.
pub(crate) fn row_uuid(value: &str, table: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("invalid UUID in {table}: {value}: {e}").into(),
        )
    })
}

pub(crate) fn row_to_track(row: &Row<'_>) -> rusqlite::Result<Track> {
    let track_type_str: String = row.get("track_type")?;
    let track_type = str_to_track_type(&track_type_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown track type: {track_type_str}").into(),
        )
    })?;
    let mut t = Track::new(
        u32::try_from(row.get::<_, i32>("stream_index")?).unwrap_or(0),
        track_type,
        row.get("codec")?,
    );
    t.language = row.get("language")?;
    t.title = row.get("title")?;
    t.is_default = row.get::<_, i32>("is_default")? != 0;
    t.is_forced = row.get::<_, i32>("is_forced")? != 0;
    t.channels = row
        .get::<_, Option<i32>>("channels")?
        .and_then(|v| u32::try_from(v).ok());
    t.channel_layout = row.get("channel_layout")?;
    t.sample_rate = row
        .get::<_, Option<i32>>("sample_rate")?
        .and_then(|v| u32::try_from(v).ok());
    t.bit_depth = row
        .get::<_, Option<i32>>("bit_depth")?
        .and_then(|v| u32::try_from(v).ok());
    t.width = row
        .get::<_, Option<i32>>("width")?
        .and_then(|v| u32::try_from(v).ok());
    t.height = row
        .get::<_, Option<i32>>("height")?
        .and_then(|v| u32::try_from(v).ok());
    t.frame_rate = row.get("frame_rate")?;
    t.is_vfr = row.get::<_, i32>("is_vfr")? != 0;
    t.is_hdr = row.get::<_, i32>("is_hdr")? != 0;
    t.hdr_format = row.get("hdr_format")?;
    t.pixel_format = row.get("pixel_format")?;
    Ok(t)
}

pub(crate) fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let status_str: String = row.get("status")?;
    let created_str: String = row.get("created_at")?;
    let started_str: Option<String> = row.get("started_at")?;
    let completed_str: Option<String> = row.get("completed_at")?;
    let payload_str: Option<String> = row.get("payload")?;
    let output_str: Option<String> = row.get("output")?;

    let id_str: String = row.get("id")?;
    let job_type = voom_domain::job::JobType::parse(&row.get::<_, String>("job_type")?);
    let mut j = Job::new(job_type);
    j.id = row_uuid(&id_str, "jobs")?;
    j.status = JobStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown job status: {status_str}").into(),
        )
    })?;
    j.priority = row.get("priority")?;
    j.payload = parse_optional_json(payload_str, "jobs.payload")?;
    j.progress = row.get("progress")?;
    j.progress_message = row.get("progress_message")?;
    j.output = parse_optional_json(output_str, "jobs.output")?;
    j.error = row.get("error")?;
    j.worker_id = row.get("worker_id")?;
    j.created_at = parse_required_datetime(created_str, "jobs.created_at")?;
    j.started_at = parse_optional_datetime(started_str, "jobs.started_at")?;
    j.completed_at = parse_optional_datetime(completed_str, "jobs.completed_at")?;
    Ok(j)
}

pub(crate) fn row_to_bad_file(row: &Row<'_>) -> rusqlite::Result<BadFile> {
    let id_str: String = row.get("id")?;
    let path_str: String = row.get("path")?;
    let error_source_str: String = row.get("error_source")?;
    let first_seen_str: String = row.get("first_seen_at")?;
    let last_seen_str: String = row.get("last_seen_at")?;

    let error_source = error_source_str.parse::<BadFileSource>().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown error_source in bad_files: {error_source_str}").into(),
        )
    })?;
    let mut bf = BadFile::new(
        PathBuf::from(path_str),
        row.get::<_, i64>("size")? as u64,
        row.get("content_hash")?,
        row.get("error")?,
        error_source,
    );
    bf.id = row_uuid(&id_str, "bad_files")?;
    bf.attempt_count = u32::try_from(row.get::<_, i64>("attempt_count")?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            format!("invalid attempt_count in bad_files: {e}").into(),
        )
    })?;
    bf.first_seen_at = parse_required_datetime(first_seen_str, "bad_files.first_seen_at")?;
    bf.last_seen_at = parse_required_datetime(last_seen_str, "bad_files.last_seen_at")?;
    Ok(bf)
}
