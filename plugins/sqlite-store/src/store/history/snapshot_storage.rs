use rusqlite::params;

use voom_domain::errors::Result;
use voom_domain::stats::{
    AudioStats, FileStats, JobAggregateStats, LibrarySnapshot, LibrarySnapshotInput,
    ProcessingAggregateStats, SnapshotTrigger, SubtitleStats, VideoStats,
};
use voom_domain::storage::SnapshotStorage;

use crate::store::{format_datetime, storage_err, SqliteStore};

impl SnapshotStorage for SqliteStore {
    fn gather_library_stats(&self, trigger: SnapshotTrigger) -> Result<LibrarySnapshot> {
        let conn = self.conn()?;

        let files = gather_file_stats(&conn)?;
        let video = gather_video_stats(&conn)?;
        let audio = gather_audio_stats(&conn)?;
        let subtitles = gather_subtitle_stats(&conn)?;
        let processing = gather_processing_stats(&conn)?;
        let jobs = gather_job_stats(&conn)?;

        Ok(LibrarySnapshot::new(LibrarySnapshotInput {
            trigger,
            files,
            video,
            audio,
            subtitles,
            processing,
            jobs,
        }))
    }

    fn save_snapshot(&self, snapshot: &LibrarySnapshot) -> Result<()> {
        let conn = self.conn()?;
        let json = serde_json::to_string(snapshot).map_err(|e| {
            voom_domain::errors::VoomError::Storage {
                kind: voom_domain::errors::StorageErrorKind::Other,
                message: format!("failed to serialize snapshot: {e}"),
            }
        })?;

        conn.execute(
            "INSERT INTO library_snapshots \
             (id, captured_at, trigger, total_files, total_size_bytes, \
              total_duration_secs, snapshot_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot.id.to_string(),
                format_datetime(&snapshot.captured_at),
                snapshot.trigger.as_str(),
                snapshot.files.total_count,
                snapshot.files.total_size_bytes,
                snapshot.files.total_duration_secs,
                json,
            ],
        )
        .map_err(storage_err("failed to save snapshot"))?;

        Ok(())
    }

    fn latest_snapshot(&self) -> Result<Option<LibrarySnapshot>> {
        let conn = self.conn()?;
        let result: Option<String> = conn
            .query_row(
                "SELECT snapshot_json FROM library_snapshots \
                 ORDER BY captured_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to query latest snapshot"))?;

        match result {
            Some(json) => {
                let snapshot: LibrarySnapshot = serde_json::from_str(&json).map_err(|e| {
                    voom_domain::errors::VoomError::Storage {
                        kind: voom_domain::errors::StorageErrorKind::Other,
                        message: format!("failed to deserialize snapshot: {e}"),
                    }
                })?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    fn list_snapshots(&self, limit: u32) -> Result<Vec<LibrarySnapshot>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT snapshot_json FROM library_snapshots \
                 ORDER BY captured_at DESC LIMIT ?1",
            )
            .map_err(storage_err("failed to prepare list snapshots"))?;

        let rows = stmt
            .query_map(params![limit.min(1000)], |row| row.get::<_, String>(0))
            .map_err(storage_err("failed to query snapshots"))?;

        let mut snapshots = Vec::new();
        for row in rows {
            let json = row.map_err(storage_err("failed to read snapshot row"))?;
            let snapshot: LibrarySnapshot = serde_json::from_str(&json).map_err(|e| {
                voom_domain::errors::VoomError::Storage {
                    kind: voom_domain::errors::StorageErrorKind::Other,
                    message: format!("failed to deserialize snapshot: {e}"),
                }
            })?;
            snapshots.push(snapshot);
        }

        Ok(snapshots)
    }

    fn prune_snapshots(&self, keep_last: u32) -> Result<u64> {
        let conn = self.conn()?;
        let deleted = conn
            .execute(
                "DELETE FROM library_snapshots WHERE id NOT IN \
                 (SELECT id FROM library_snapshots \
                  ORDER BY captured_at DESC LIMIT ?1)",
                params![keep_last],
            )
            .map_err(storage_err("failed to prune snapshots"))?;

        Ok(deleted as u64)
    }
}

/// Query helper: run a GROUP BY query returning `(String, u64)` pairs.
fn query_distribution(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
) -> Result<Vec<(String, u64)>> {
    let mut stmt = conn
        .prepare(sql)
        .map_err(storage_err("failed to prepare distribution query"))?;

    let rows = stmt
        .query_map(params, |row| {
            let key: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            Ok((key, count as u64))
        })
        .map_err(storage_err("failed to query distribution"))?;

    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err("failed to collect distribution"))
}

fn gather_file_stats(conn: &rusqlite::Connection) -> Result<FileStats> {
    let mut stats = conn
        .query_row(
            "SELECT COUNT(*), \
                    COALESCE(SUM(size), 0), \
                    COALESCE(SUM(duration), 0.0), \
                    COALESCE(AVG(size), 0), \
                    COALESCE(AVG(duration), 0.0), \
                    COALESCE(MAX(size), 0), \
                    COALESCE(MIN(size), 0) \
             FROM files WHERE status = 'active'",
            [],
            |row| {
                let mut s = FileStats::default();
                s.total_count = row.get::<_, i64>(0)? as u64;
                s.total_size_bytes = row.get::<_, i64>(1)? as u64;
                s.total_duration_secs = row.get(2)?;
                s.avg_size_bytes = row.get::<_, f64>(3)? as u64;
                s.avg_duration_secs = row.get(4)?;
                s.max_size_bytes = row.get::<_, i64>(5)? as u64;
                s.min_size_bytes = row.get::<_, i64>(6)? as u64;
                Ok(s)
            },
        )
        .map_err(storage_err("failed to query file aggregates"))?;

    stats.container_counts = query_distribution(
        conn,
        "SELECT container, COUNT(*) FROM files \
         WHERE status = 'active' \
         GROUP BY container ORDER BY COUNT(*) DESC",
        &[],
    )?;

    Ok(stats)
}

fn gather_video_stats(conn: &rusqlite::Connection) -> Result<VideoStats> {
    let total_tracks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tracks \
             INNER JOIN files ON tracks.file_id = files.id \
             WHERE files.status = 'active' AND track_type = 'video'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err("failed to count video tracks"))?;

    let hdr_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tracks \
             INNER JOIN files ON tracks.file_id = files.id \
             WHERE files.status = 'active' \
             AND track_type = 'video' AND is_hdr = 1",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err("failed to count HDR tracks"))?;

    let vfr_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tracks \
             INNER JOIN files ON tracks.file_id = files.id \
             WHERE files.status = 'active' \
             AND track_type = 'video' AND is_vfr = 1",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err("failed to count VFR tracks"))?;

    let codec_counts = query_distribution(
        conn,
        "SELECT codec, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type = 'video' \
         GROUP BY codec ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let resolution_counts = query_distribution(
        conn,
        "SELECT CASE \
             WHEN width >= 7680 THEN '8K' \
             WHEN width >= 3840 THEN '4K' \
             WHEN width >= 1920 THEN '1080p' \
             WHEN width >= 1280 THEN '720p' \
             ELSE 'SD' \
         END AS res, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' \
         AND track_type = 'video' AND width IS NOT NULL \
         GROUP BY res ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let hdr_format_counts = query_distribution(
        conn,
        "SELECT COALESCE(hdr_format, 'unknown'), COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' \
         AND track_type = 'video' AND is_hdr = 1 \
         GROUP BY hdr_format ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let frame_rate_counts = query_distribution(
        conn,
        "SELECT CASE \
             WHEN frame_rate IS NULL THEN 'unknown' \
             WHEN ABS(frame_rate - 23.976) < 0.1 THEN '23.976' \
             WHEN ABS(frame_rate - 24.0) < 0.1 THEN '24' \
             WHEN ABS(frame_rate - 25.0) < 0.1 THEN '25' \
             WHEN ABS(frame_rate - 29.97) < 0.1 THEN '29.97' \
             WHEN ABS(frame_rate - 30.0) < 0.1 THEN '30' \
             WHEN ABS(frame_rate - 50.0) < 0.1 THEN '50' \
             WHEN ABS(frame_rate - 59.94) < 0.1 THEN '59.94' \
             WHEN ABS(frame_rate - 60.0) < 0.1 THEN '60' \
             ELSE CAST(ROUND(frame_rate, 1) AS TEXT) \
         END AS fps, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type = 'video' \
         GROUP BY fps ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let pixel_format_counts = query_distribution(
        conn,
        "SELECT COALESCE(pixel_format, 'unknown'), COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type = 'video' \
         GROUP BY pixel_format ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let mut stats = VideoStats::default();
    stats.total_tracks = total_tracks as u64;
    stats.codec_counts = codec_counts;
    stats.resolution_counts = resolution_counts;
    stats.hdr_count = hdr_count as u64;
    stats.hdr_format_counts = hdr_format_counts;
    stats.frame_rate_counts = frame_rate_counts;
    stats.vfr_count = vfr_count as u64;
    stats.pixel_format_counts = pixel_format_counts;
    Ok(stats)
}

fn gather_audio_stats(conn: &rusqlite::Connection) -> Result<AudioStats> {
    let total_tracks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tracks \
             INNER JOIN files ON tracks.file_id = files.id \
             WHERE files.status = 'active' AND track_type LIKE 'audio%'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err("failed to count audio tracks"))?;

    let type_counts = query_distribution(
        conn,
        "SELECT track_type, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY track_type ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let language_counts = query_distribution(
        conn,
        "SELECT language, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY language ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let codec_counts = query_distribution(
        conn,
        "SELECT codec, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY codec ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let channel_layout_counts = query_distribution(
        conn,
        "SELECT COALESCE(channel_layout, 'unknown'), COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY channel_layout ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let sample_rate_counts = query_distribution(
        conn,
        "SELECT COALESCE(CAST(sample_rate AS TEXT), 'unknown'), COUNT(*) \
         FROM tracks INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY sample_rate ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let bit_depth_counts = query_distribution(
        conn,
        "SELECT COALESCE(CAST(bit_depth AS TEXT), 'unknown'), COUNT(*) \
         FROM tracks INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' AND track_type LIKE 'audio%' \
         GROUP BY bit_depth ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let mut stats = AudioStats::default();
    stats.total_tracks = total_tracks as u64;
    stats.type_counts = type_counts;
    stats.language_counts = language_counts;
    stats.codec_counts = codec_counts;
    stats.channel_layout_counts = channel_layout_counts;
    stats.sample_rate_counts = sample_rate_counts;
    stats.bit_depth_counts = bit_depth_counts;
    Ok(stats)
}

fn gather_subtitle_stats(conn: &rusqlite::Connection) -> Result<SubtitleStats> {
    let total_tracks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tracks \
             INNER JOIN files ON tracks.file_id = files.id \
             WHERE files.status = 'active' \
             AND track_type LIKE 'subtitle%'",
            [],
            |row| row.get(0),
        )
        .map_err(storage_err("failed to count subtitle tracks"))?;

    let language_counts = query_distribution(
        conn,
        "SELECT language, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' \
         AND track_type LIKE 'subtitle%' \
         GROUP BY language ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let type_counts = query_distribution(
        conn,
        "SELECT track_type, COUNT(*) FROM tracks \
         INNER JOIN files ON tracks.file_id = files.id \
         WHERE files.status = 'active' \
         AND track_type LIKE 'subtitle%' \
         GROUP BY track_type ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let external_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM subtitles", [], |row| row.get(0))
        .map_err(storage_err("failed to count external subtitles"))?;

    let mut stats = SubtitleStats::default();
    stats.total_tracks = total_tracks as u64;
    stats.language_counts = language_counts;
    stats.type_counts = type_counts;
    stats.external_count = external_count as u64;
    Ok(stats)
}

fn gather_processing_stats(conn: &rusqlite::Connection) -> Result<ProcessingAggregateStats> {
    let plans_by_status = query_distribution(
        conn,
        "SELECT status, COUNT(*) FROM plans \
         GROUP BY status ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let outcomes = query_distribution(
        conn,
        "SELECT outcome, COUNT(*) FROM file_transitions \
         WHERE outcome IS NOT NULL \
         GROUP BY outcome ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let (total_time, total_saved) = conn
        .query_row(
            "SELECT COALESCE(SUM(duration_ms), 0), \
                    COALESCE(SUM(CASE WHEN from_size IS NOT NULL \
                        THEN from_size - to_size ELSE 0 END), 0) \
             FROM file_transitions \
             WHERE source = 'voom' AND outcome = 'success'",
            [],
            |row| {
                let time: i64 = row.get(0)?;
                let saved: i64 = row.get(1)?;
                Ok((time, saved))
            },
        )
        .map_err(storage_err("failed to query processing aggregates"))?;

    let bad_file_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM bad_files", [], |row| row.get(0))
        .map_err(storage_err("failed to count bad files"))?;

    let bad_files_by_source = query_distribution(
        conn,
        "SELECT error_source, COUNT(*) FROM bad_files \
         GROUP BY error_source ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let mut stats = ProcessingAggregateStats::default();
    stats.plans_by_status = plans_by_status;
    stats.outcomes = outcomes;
    stats.total_processing_time_ms = total_time as u64;
    stats.total_size_saved_bytes = total_saved;
    stats.bad_file_count = bad_file_count as u64;
    stats.bad_files_by_source = bad_files_by_source;
    Ok(stats)
}

fn gather_job_stats(conn: &rusqlite::Connection) -> Result<JobAggregateStats> {
    let by_status = query_distribution(
        conn,
        "SELECT status, COUNT(*) FROM jobs \
         GROUP BY status ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let by_type = query_distribution(
        conn,
        "SELECT job_type, COUNT(*) FROM jobs \
         GROUP BY job_type ORDER BY COUNT(*) DESC",
        &[],
    )?;

    let mut stats = JobAggregateStats::default();
    stats.by_status = by_status;
    stats.by_type = by_type;
    Ok(stats)
}

use crate::store::OptionalExt;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::storage::FileStorage;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    fn sample_file_with_tracks() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/movies/test.mkv"));
        file.size = 1_500_000_000;
        file.content_hash = Some("abc123".to_string());
        file.container = Container::Mkv;
        file.duration = 7200.0;
        file.tracks = vec![
            {
                let mut t = Track::new(0, TrackType::Video, "hevc".into());
                t.width = Some(1920);
                t.height = Some(1080);
                t.is_hdr = true;
                t.hdr_format = Some("HDR10".into());
                t.frame_rate = Some(23.976);
                t.pixel_format = Some("yuv420p10le".into());
                t
            },
            {
                let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
                t.language = "eng".into();
                t.channels = Some(6);
                t.channel_layout = Some("5.1".into());
                t.sample_rate = Some(48000);
                t.is_default = true;
                t
            },
            {
                let mut t = Track::new(2, TrackType::AudioAlternate, "opus".into());
                t.language = "jpn".into();
                t.channels = Some(2);
                t.channel_layout = Some("stereo".into());
                t.sample_rate = Some(48000);
                t
            },
            {
                let mut t = Track::new(3, TrackType::SubtitleMain, "srt".into());
                t.language = "eng".into();
                t
            },
        ];
        file
    }

    #[test]
    fn test_gather_empty_database() {
        let store = test_store();
        let snapshot = store.gather_library_stats(SnapshotTrigger::Manual).unwrap();

        assert_eq!(snapshot.files.total_count, 0);
        assert_eq!(snapshot.files.total_size_bytes, 0);
        assert_eq!(snapshot.video.total_tracks, 0);
        assert_eq!(snapshot.audio.total_tracks, 0);
        assert_eq!(snapshot.subtitles.total_tracks, 0);
        assert_eq!(snapshot.trigger, SnapshotTrigger::Manual);
    }

    #[test]
    fn test_gather_with_files() {
        let store = test_store();
        let file = sample_file_with_tracks();
        store.upsert_file(&file).unwrap();

        let snapshot = store
            .gather_library_stats(SnapshotTrigger::ScanComplete)
            .unwrap();

        assert_eq!(snapshot.files.total_count, 1);
        assert_eq!(snapshot.files.total_size_bytes, 1_500_000_000);
        assert_eq!(snapshot.files.total_duration_secs, 7200.0);
        assert_eq!(snapshot.files.max_size_bytes, 1_500_000_000);
        assert_eq!(snapshot.files.min_size_bytes, 1_500_000_000);
        assert_eq!(snapshot.files.container_counts.len(), 1);
        assert_eq!(snapshot.files.container_counts[0].0, "mkv");

        assert_eq!(snapshot.video.total_tracks, 1);
        assert_eq!(snapshot.video.codec_counts[0].0, "hevc");
        assert_eq!(snapshot.video.hdr_count, 1);
        assert_eq!(snapshot.video.resolution_counts[0].0, "1080p");

        assert_eq!(snapshot.audio.total_tracks, 2);
        assert_eq!(snapshot.subtitles.total_tracks, 1);
    }

    #[test]
    fn test_save_and_load_snapshot() {
        let store = test_store();
        let snapshot = store.gather_library_stats(SnapshotTrigger::Manual).unwrap();

        store.save_snapshot(&snapshot).unwrap();

        let loaded = store.latest_snapshot().unwrap().unwrap();
        assert_eq!(loaded.id, snapshot.id);
        assert_eq!(loaded.trigger, SnapshotTrigger::Manual);
    }

    #[test]
    fn test_list_snapshots() {
        let store = test_store();

        for _ in 0..3 {
            let snapshot = store.gather_library_stats(SnapshotTrigger::Manual).unwrap();
            store.save_snapshot(&snapshot).unwrap();
        }

        let all = store.list_snapshots(10).unwrap();
        assert_eq!(all.len(), 3);

        let limited = store.list_snapshots(2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn test_prune_snapshots() {
        let store = test_store();

        for _ in 0..5 {
            let snapshot = store.gather_library_stats(SnapshotTrigger::Manual).unwrap();
            store.save_snapshot(&snapshot).unwrap();
        }

        let pruned = store.prune_snapshots(2).unwrap();
        assert_eq!(pruned, 3);

        let remaining = store.list_snapshots(10).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_latest_snapshot_empty() {
        let store = test_store();
        let result = store.latest_snapshot().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_missing_files_excluded_from_stats() {
        let store = test_store();

        let active_file = sample_file_with_tracks();
        store.upsert_file(&active_file).unwrap();

        let mut missing_file = MediaFile::new(PathBuf::from("/media/movies/gone.mkv"));
        missing_file.size = 9_000_000_000;
        missing_file.content_hash = Some("dead".to_string());
        missing_file.container = Container::Mkv;
        missing_file.duration = 99999.0;
        missing_file.tracks = vec![
            {
                let mut t = Track::new(0, TrackType::Video, "av1".into());
                t.width = Some(3840);
                t.height = Some(2160);
                t
            },
            {
                let mut t = Track::new(1, TrackType::AudioMain, "ac3".into());
                t.language = "deu".into();
                t
            },
            {
                let mut t = Track::new(2, TrackType::SubtitleMain, "ass".into());
                t.language = "deu".into();
                t
            },
        ];
        store.upsert_file(&missing_file).unwrap();
        store.mark_missing(&missing_file.id).unwrap();

        let snapshot = store
            .gather_library_stats(SnapshotTrigger::ScanComplete)
            .unwrap();

        // Only the active file's data should appear in stats.
        assert_eq!(snapshot.files.total_count, 1);
        assert_eq!(snapshot.files.total_size_bytes, active_file.size);
        assert_eq!(snapshot.files.total_duration_secs, active_file.duration);
        // Container counts should only include the active file's container.
        assert_eq!(snapshot.files.container_counts.len(), 1);
        assert_eq!(snapshot.files.container_counts[0].1, 1);

        // Track stats should exclude the missing file's tracks.
        assert_eq!(snapshot.video.total_tracks, 1);
        assert_eq!(snapshot.video.codec_counts[0].0, "hevc");
        assert_eq!(snapshot.audio.total_tracks, 2);
        assert_eq!(snapshot.subtitles.total_tracks, 1);
    }
}
