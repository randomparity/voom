use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::types::Type;
use rusqlite::Row;
use uuid::Uuid;

use voom_domain::bad_file::{BadFile, BadFileSource};
use voom_domain::errors::{Result, StorageErrorKind, VoomError};
use voom_domain::job::{Job, JobStatus};
use voom_domain::media::{Container, CropDetection, CropRect, MediaFile, Track, TrackType};
use voom_domain::transition::FileStatus;
use voom_domain::verification::{
    VerificationMode, VerificationOutcome, VerificationRecord, VerificationRecordInput,
};

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

pub(crate) fn checked_i64_to_u64(value: i64, field: &str) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|e| numeric_conversion_failure(field, e))
}

pub(crate) fn checked_optional_i64_to_u64(
    value: Option<i64>,
    field: &str,
) -> rusqlite::Result<Option<u64>> {
    value.map(|v| checked_i64_to_u64(v, field)).transpose()
}

pub(crate) fn checked_i64_to_u32(value: i64, field: &str) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|e| numeric_conversion_failure(field, e))
}

pub(crate) fn checked_optional_i64_to_u32(
    value: Option<i64>,
    field: &str,
) -> rusqlite::Result<Option<u32>> {
    value.map(|v| checked_i64_to_u32(v, field)).transpose()
}

fn checked_i32_to_u32(value: i32, field: &str) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|e| numeric_conversion_failure(field, e))
}

fn checked_optional_i32_to_u32(value: Option<i32>, field: &str) -> rusqlite::Result<Option<u32>> {
    value.map(|v| checked_i32_to_u32(v, field)).transpose()
}

fn numeric_conversion_failure(
    field: &str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        Type::Integer,
        format!("invalid numeric value in {field}: {error}").into(),
    )
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
    let size = checked_i64_to_u64(row.get("size")?, "files.size")?;
    let bitrate = checked_optional_i32_to_u32(row.get("bitrate")?, "files.bitrate")?;

    Ok(FileRow {
        id: row.get("id")?,
        path: row.get::<_, Option<String>>("path")?.unwrap_or_default(),
        size,
        content_hash: row.get("content_hash")?,
        expected_hash: row.get("expected_hash")?,
        status: row.get("status")?,
        container: row.get("container")?,
        duration: row.get("duration")?,
        bitrate,
        crop_left: row.get("crop_left")?,
        crop_top: row.get("crop_top")?,
        crop_right: row.get("crop_right")?,
        crop_bottom: row.get("crop_bottom")?,
        crop_detected_at: row.get("crop_detected_at")?,
        crop_settings_fingerprint: row.get("crop_settings_fingerprint")?,
        tags: row.get("tags")?,
        plugin_metadata: row.get("plugin_metadata")?,
        introspected_at: row.get("introspected_at")?,
    })
}

pub(crate) struct FileRow {
    pub(crate) id: String,
    path: String,
    size: u64,
    content_hash: String,
    expected_hash: Option<String>,
    status: String,
    container: String,
    duration: Option<f64>,
    bitrate: Option<u32>,
    crop_left: Option<i64>,
    crop_top: Option<i64>,
    crop_right: Option<i64>,
    crop_bottom: Option<i64>,
    crop_detected_at: Option<String>,
    crop_settings_fingerprint: Option<String>,
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
        mf.size = self.size;
        mf.content_hash = if self.content_hash.is_empty() {
            None
        } else {
            Some(self.content_hash.clone())
        };
        mf.container = Container::from_extension(&self.container);
        mf.duration = self.duration.unwrap_or(0.0);
        mf.bitrate = self.bitrate;
        mf.crop_detection = self.crop_detection()?;
        mf.tracks = tracks;
        mf.tags = tags;
        mf.plugin_metadata = plugin_metadata;
        mf.introspected_at = parse_datetime(&self.introspected_at)?;
        mf.expected_hash = self.expected_hash.clone();
        mf.status = parse_file_status(&self.status)?;
        Ok(mf)
    }
}

impl FileRow {
    fn crop_detection(&self) -> Result<Option<CropDetection>> {
        let Some(detected_at) = &self.crop_detected_at else {
            return Ok(None);
        };
        let Some(left) = self.crop_left else {
            return Ok(None);
        };
        let Some(top) = self.crop_top else {
            return Ok(None);
        };
        let Some(right) = self.crop_right else {
            return Ok(None);
        };
        let Some(bottom) = self.crop_bottom else {
            return Ok(None);
        };
        let rect = CropRect::new(
            u32::try_from(left).map_err(other_storage_err("invalid files.crop_left"))?,
            u32::try_from(top).map_err(other_storage_err("invalid files.crop_top"))?,
            u32::try_from(right).map_err(other_storage_err("invalid files.crop_right"))?,
            u32::try_from(bottom).map_err(other_storage_err("invalid files.crop_bottom"))?,
        );
        let detected_at = parse_datetime(detected_at)?;
        let detection = CropDetection::new(rect, detected_at);
        let detection = match &self.crop_settings_fingerprint {
            Some(fingerprint) => detection.with_settings_fingerprint(fingerprint.clone()),
            None => detection,
        };
        Ok(Some(detection))
    }
}

fn parse_file_status(value: &str) -> Result<FileStatus> {
    FileStatus::parse(value).ok_or_else(|| VoomError::Storage {
        kind: StorageErrorKind::Other,
        message: format!("unknown file status in files.status: {value}"),
    })
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
    let stream_index = checked_i32_to_u32(row.get("stream_index")?, "tracks.stream_index")?;
    let channels = checked_optional_i32_to_u32(row.get("channels")?, "tracks.channels")?;
    let sample_rate = checked_optional_i32_to_u32(row.get("sample_rate")?, "tracks.sample_rate")?;
    let bit_depth = checked_optional_i32_to_u32(row.get("bit_depth")?, "tracks.bit_depth")?;
    let width = checked_optional_i32_to_u32(row.get("width")?, "tracks.width")?;
    let height = checked_optional_i32_to_u32(row.get("height")?, "tracks.height")?;

    let mut t = Track::new(stream_index, track_type, row.get("codec")?);
    t.language = row.get("language")?;
    t.title = row.get("title")?;
    t.is_default = row.get::<_, i32>("is_default")? != 0;
    t.is_forced = row.get::<_, i32>("is_forced")? != 0;
    t.channels = channels;
    t.channel_layout = row.get("channel_layout")?;
    t.sample_rate = sample_rate;
    t.bit_depth = bit_depth;
    t.loudness_integrated_lufs = row.get("loudness_integrated_lufs")?;
    t.loudness_true_peak_db = row.get("loudness_true_peak_db")?;
    t.loudness_range_lu = row.get("loudness_range_lu")?;
    let measured_at: Option<String> = row.get("loudness_measured_at")?;
    t.loudness_measured_at = parse_optional_datetime(measured_at, "tracks.loudness_measured_at")?;
    t.width = width;
    t.height = height;
    t.frame_rate = row.get("frame_rate")?;
    t.is_vfr = row.get::<_, i32>("is_vfr")? != 0;
    t.is_hdr = row.get::<_, i32>("is_hdr")? != 0;
    t.hdr_format = row.get("hdr_format")?;
    t.pixel_format = row.get("pixel_format")?;
    t.color_primaries = row.get("color_primaries")?;
    t.color_transfer = row.get("color_transfer")?;
    t.color_matrix = row.get("color_matrix")?;
    t.max_cll = checked_optional_i32_to_u32(row.get("max_cll")?, "tracks.max_cll")?;
    t.max_fall = checked_optional_i32_to_u32(row.get("max_fall")?, "tracks.max_fall")?;
    t.master_display = row.get("master_display")?;
    t.dolby_vision_profile = row
        .get::<_, Option<i32>>("dolby_vision_profile")?
        .map(|value| {
            u8::try_from(value)
                .map_err(|e| numeric_conversion_failure("tracks.dolby_vision_profile", e))
        })
        .transpose()?;
    t.is_animation = row.get::<_, Option<i32>>("is_animation")?.map(|v| v != 0);
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
        checked_i64_to_u64(row.get("size")?, "bad_files.size")?,
        row.get("content_hash")?,
        row.get("error")?,
        error_source,
    );
    bf.id = row_uuid(&id_str, "bad_files")?;
    bf.attempt_count = checked_i64_to_u32(row.get("attempt_count")?, "bad_files.attempt_count")?;
    bf.first_seen_at = parse_required_datetime(first_seen_str, "bad_files.first_seen_at")?;
    bf.last_seen_at = parse_required_datetime(last_seen_str, "bad_files.last_seen_at")?;
    Ok(bf)
}

pub(crate) fn row_to_verification(row: &Row<'_>) -> rusqlite::Result<VerificationRecord> {
    let id_str: String = row.get("id")?;
    let id = row_uuid(&id_str, "verifications")?;
    let file_id: String = row.get("file_id")?;
    let verified_at_str: String = row.get("verified_at")?;
    let verified_at = parse_required_datetime(verified_at_str, "verifications.verified_at")?;
    let mode_str: String = row.get("mode")?;
    let mode = VerificationMode::parse(&mode_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown verification mode: {mode_str}").into(),
        )
    })?;
    let outcome_str: String = row.get("outcome")?;
    let outcome = VerificationOutcome::parse(&outcome_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown verification outcome: {outcome_str}").into(),
        )
    })?;
    let error_count = checked_i64_to_u32(row.get("error_count")?, "verifications.error_count")?;
    let warning_count =
        checked_i64_to_u32(row.get("warning_count")?, "verifications.warning_count")?;
    Ok(VerificationRecord::new(VerificationRecordInput {
        id,
        file_id,
        verified_at,
        mode,
        outcome,
        error_count,
        warning_count,
        content_hash: row.get("content_hash")?,
        details: row.get("details")?,
    }))
}

#[cfg(test)]
mod tests {
    // The `row_to_*` functions take a live `rusqlite::Row<'_>` that cannot be
    // easily hand-crafted in a unit test — those paths are exercised end-to-end
    // by the per-entity storage tests. Below we cover the pure helpers
    // `parse_required_datetime`, `parse_optional_datetime`, `parse_optional_json`,
    // `row_uuid`, and `str_to_track_type`.
    use super::*;
    use voom_domain::media::TrackType;

    #[test]
    fn parse_required_datetime_accepts_rfc3339() {
        let s = "2024-01-02T03:04:05Z".to_string();
        let parsed = parse_required_datetime(s, "test.field").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2024-01-02T03:04:05+00:00");
    }

    #[test]
    fn parse_required_datetime_rejects_garbage() {
        let s = "definitely-not-a-date".to_string();
        let err = parse_required_datetime(s, "test.field").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("test.field"),
            "error should mention field: {msg}"
        );
    }

    #[test]
    fn parse_optional_datetime_passes_through_none() {
        let parsed = parse_optional_datetime(None, "test.field").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_optional_datetime_accepts_valid() {
        let parsed =
            parse_optional_datetime(Some("2024-06-15T12:00:00Z".into()), "test.field").unwrap();
        assert!(parsed.is_some());
    }

    #[test]
    fn parse_optional_datetime_rejects_garbage() {
        let err = parse_optional_datetime(Some("not-a-date".into()), "test.field").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("test.field"));
    }

    #[test]
    fn parse_optional_json_roundtrips_valid_value() {
        let parsed = parse_optional_json(Some(r#"{"key":42}"#.to_string()), "test.field").unwrap();
        assert_eq!(parsed.unwrap()["key"], 42);
    }

    #[test]
    fn parse_optional_json_passes_through_none() {
        let parsed = parse_optional_json(None, "test.field").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn parse_optional_json_rejects_malformed() {
        let err = parse_optional_json(Some("not json{".into()), "test.field").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("test.field"));
    }

    #[test]
    fn checked_i64_to_u64_accepts_non_negative_values() {
        assert_eq!(checked_i64_to_u64(42, "files.size").unwrap(), 42);
    }

    #[test]
    fn checked_i64_to_u64_rejects_negative_values_with_column_context() {
        let err = checked_i64_to_u64(-1, "files.size").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("files.size"),
            "error should mention column: {msg}"
        );
    }

    #[test]
    fn checked_i64_to_u32_rejects_overflow_with_column_context() {
        let err = checked_i64_to_u32(i64::from(u32::MAX) + 1, "tracks.stream_index").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("tracks.stream_index"),
            "error should mention column: {msg}"
        );
    }

    #[test]
    fn row_uuid_parses_valid_string() {
        let s = "550e8400-e29b-41d4-a716-446655440000";
        let parsed = row_uuid(s, "my_table").unwrap();
        assert_eq!(parsed.to_string(), s);
    }

    #[test]
    fn row_uuid_rejects_invalid_string() {
        let err = row_uuid("not-a-uuid", "my_table").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("my_table"),
            "error should mention table: {msg}"
        );
    }

    #[test]
    fn str_to_track_type_known_values() {
        assert_eq!(str_to_track_type("video"), Some(TrackType::Video));
        assert_eq!(str_to_track_type("audio_main"), Some(TrackType::AudioMain));
        assert_eq!(
            str_to_track_type("subtitle_main"),
            Some(TrackType::SubtitleMain)
        );
        assert_eq!(str_to_track_type("attachment"), Some(TrackType::Attachment));
    }

    #[test]
    fn str_to_track_type_unknown_returns_none() {
        assert_eq!(str_to_track_type("not-a-type"), None);
        assert_eq!(str_to_track_type(""), None);
    }
}
