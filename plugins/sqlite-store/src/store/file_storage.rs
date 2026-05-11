use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::media::{MediaFile, StoredFingerprint};
use voom_domain::storage::{FileFilters, FileStorage};
use voom_domain::transition::{
    DiscoveredFile, FileStatus, FileTransition, IngestDecision, ReconcileResult, ScanFinishOutcome,
    TransitionSource,
};

use super::{
    FileRow, OptionalExt, SqlQuery, SqliteStore, escape_like, format_datetime, other_storage_err,
    parse_datetime, row_to_file, storage_err,
};

fn filename_string(path: &Path) -> String {
    path.file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default()
}

impl FileStorage for SqliteStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let tags_json = serde_json::to_string(&file.tags)
            .map_err(other_storage_err("failed to serialize tags"))?;
        let meta_json = serde_json::to_string(&file.plugin_metadata)
            .map_err(other_storage_err("failed to serialize metadata"))?;
        let filename = filename_string(&file.path);
        let path_str = file.path.to_string_lossy().to_string();

        // Preserve existing file ID on re-scan to avoid orphaning related records
        let existing_id: Option<String> = conn
            .query_row(
                "SELECT id FROM files WHERE path = ?1",
                params![&path_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to query existing file"))?;

        let effective_id = existing_id.clone().unwrap_or_else(|| file.id.to_string());

        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin transaction"))?;

        // Delete old tracks before upserting
        tx.execute(
            "DELETE FROM tracks WHERE file_id IN (SELECT id FROM files WHERE path = ?1)",
            params![&path_str],
        )
        .map_err(storage_err("failed to delete old tracks"))?;

        tx.execute(
            "INSERT INTO files (id, path, filename, size, content_hash, status, container, duration, bitrate, crop_left, crop_top, crop_right, crop_bottom, crop_detected_at, crop_settings_fingerprint, tags, plugin_metadata, introspected_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT(path) DO UPDATE SET
                filename = excluded.filename,
                size = excluded.size,
                content_hash = excluded.content_hash,
                status = excluded.status,
                container = excluded.container,
                duration = excluded.duration,
                bitrate = excluded.bitrate,
                crop_left = excluded.crop_left,
                crop_top = excluded.crop_top,
                crop_right = excluded.crop_right,
                crop_bottom = excluded.crop_bottom,
                crop_detected_at = excluded.crop_detected_at,
                crop_settings_fingerprint = excluded.crop_settings_fingerprint,
                tags = excluded.tags,
                plugin_metadata = excluded.plugin_metadata,
                introspected_at = excluded.introspected_at,
                updated_at = excluded.updated_at",
            params![
                &effective_id,
                &path_str,
                filename,
                file.size as i64,
                file.content_hash.as_deref().unwrap_or(""),
                file.status.as_str(),
                file.container.as_str(),
                file.duration,
                file.bitrate.map(i64::from),
                file.crop_detection.as_ref().map(|d| i64::from(d.rect.left)),
                file.crop_detection.as_ref().map(|d| i64::from(d.rect.top)),
                file.crop_detection.as_ref().map(|d| i64::from(d.rect.right)),
                file.crop_detection
                    .as_ref()
                    .map(|d| i64::from(d.rect.bottom)),
                file.crop_detection
                    .as_ref()
                    .map(|d| format_datetime(&d.detected_at)),
                file.crop_detection
                    .as_ref()
                    .and_then(|d| d.settings_fingerprint.as_deref()),
                tags_json,
                meta_json,
                format_datetime(&file.introspected_at),
                &now,
                &now,
            ],
        )
        .map_err(storage_err("failed to upsert file"))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO tracks (id, file_id, stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, loudness_integrated_lufs, loudness_true_peak_db, loudness_range_lu, loudness_measured_at, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format, color_primaries, color_transfer, color_matrix, max_cll, max_fall, master_display, dolby_vision_profile, is_animation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32)",
                )
                .map_err(storage_err("failed to prepare track insert"))?;

            for track in &file.tracks {
                stmt.execute(params![
                    Uuid::new_v4().to_string(),
                    &effective_id,
                    i64::from(track.index),
                    track.track_type.as_str(),
                    track.codec,
                    track.language,
                    track.title,
                    i64::from(track.is_default),
                    i64::from(track.is_forced),
                    track.channels.map(i64::from),
                    track.channel_layout,
                    track.sample_rate.map(i64::from),
                    track.bit_depth.map(i64::from),
                    track.loudness_integrated_lufs,
                    track.loudness_true_peak_db,
                    track.loudness_range_lu,
                    track.loudness_measured_at.map(|dt| dt.to_rfc3339()),
                    track.width.map(i64::from),
                    track.height.map(i64::from),
                    track.frame_rate,
                    i64::from(track.is_vfr),
                    i64::from(track.is_hdr),
                    track.hdr_format,
                    track.pixel_format,
                    track.color_primaries,
                    track.color_transfer,
                    track.color_matrix,
                    track.max_cll.map(i64::from),
                    track.max_fall.map(i64::from),
                    track.master_display,
                    track.dolby_vision_profile.map(i64::from),
                    track.is_animation.map(i64::from),
                ])
                .map_err(storage_err("failed to insert track"))?;
            }
        }

        tx.commit().map_err(storage_err("failed to commit"))?;
        Ok(())
    }

    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, crop_left, crop_top, crop_right, crop_bottom, crop_detected_at, crop_settings_fingerprint, tags, plugin_metadata, introspected_at FROM files WHERE id = ?1",
                params![id.to_string()],
                row_to_file,
            )
            .optional()
            .map_err(storage_err("failed to get file"))?;

        match file_row {
            Some(fr) => {
                let tracks = self.load_tracks(&conn, id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
    }

    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, crop_left, crop_top, crop_right, crop_bottom, crop_detected_at, crop_settings_fingerprint, tags, plugin_metadata, introspected_at FROM files WHERE path = ?1",
                params![path_str],
                row_to_file,
            )
            .optional()
            .map_err(storage_err("failed to get file by path"))?;

        match file_row {
            Some(fr) => {
                let id = super::parse_uuid(&fr.id)?;
                let tracks = self.load_tracks(&conn, &id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
    }

    fn file_fingerprint_by_path(&self, path: &Path) -> Result<Option<StoredFingerprint>> {
        let conn = self.conn()?;
        let path_str = path.to_string_lossy().to_string();
        let row: Option<(i64, Option<String>, String)> = conn
            .query_row(
                "SELECT size, content_hash, introspected_at FROM files WHERE path = ?1",
                params![path_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(storage_err("failed to get file fingerprint by path"))?;
        let Some((size, content_hash, introspected_at)) = row else {
            return Ok(None);
        };
        // `upsert_file` writes an empty string when the caller has no hash,
        // so treat empty as equivalent to NULL.
        let content_hash = match content_hash {
            Some(h) if !h.is_empty() => h,
            _ => return Ok(None),
        };
        let last_seen = parse_datetime(&introspected_at)?;
        let size =
            u64::try_from(size).map_err(other_storage_err("file size does not fit in u64"))?;
        Ok(Some(StoredFingerprint {
            size,
            content_hash,
            last_seen,
        }))
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let conn = self.conn()?;
        let has_track_filter = filters.has_codec.is_some() || filters.has_language.is_some();

        // When filtering by codec/language, use a subquery to apply filters before
        // LIMIT/OFFSET, ensuring consistent pagination with count_files.
        let base = if has_track_filter {
            "SELECT DISTINCT files.id, files.path, files.size, files.content_hash, files.expected_hash, files.status, files.container, files.duration, files.bitrate, files.crop_left, files.crop_top, files.crop_right, files.crop_bottom, files.crop_detected_at, files.crop_settings_fingerprint, files.tags, files.plugin_metadata, files.introspected_at FROM files INNER JOIN tracks ON tracks.file_id = files.id WHERE 1=1"
        } else {
            "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, crop_left, crop_top, crop_right, crop_bottom, crop_detected_at, crop_settings_fingerprint, tags, plugin_metadata, introspected_at FROM files WHERE 1=1"
        };
        let mut q = SqlQuery::new(base);

        let col_prefix = if has_track_filter { "files." } else { "" };

        if !filters.include_missing {
            let clause = format!(" AND {col_prefix}status = {{}}");
            q.condition(&clause, FileStatus::Active.as_str().to_string());
        }
        if let Some(container) = filters.container {
            let clause = format!(" AND {col_prefix}container = {{}}");
            q.condition(&clause, container.as_str().to_string());
        }
        if let Some(ref prefix) = filters.path_prefix {
            let clause = format!(" AND {col_prefix}path LIKE {{}} ESCAPE '\\'");
            q.condition(&clause, format!("{}%", escape_like(prefix)));
        }
        if let Some(ref codec) = filters.has_codec {
            q.condition(" AND tracks.codec = {}", codec.clone());
        }
        if let Some(ref lang) = filters.has_language {
            q.condition(" AND tracks.language = {}", lang.clone());
        }

        q.sql.push_str(&format!(" ORDER BY {col_prefix}path"));

        q.paginate(filters.limit, filters.offset);

        let mut stmt = conn
            .prepare(&q.sql)
            .map_err(storage_err("failed to prepare list query"))?;

        let rows: Vec<FileRow> = stmt
            .query_map(q.param_refs().as_slice(), row_to_file)
            .map_err(storage_err("failed to list files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect files"))?;

        let file_ids: Vec<Uuid> = rows
            .iter()
            .map(|fr| super::parse_uuid(&fr.id))
            .collect::<Result<Vec<_>>>()?;
        let tracks_map = self.load_tracks_batch(&conn, &file_ids)?;

        let mut files = Vec::with_capacity(rows.len());
        for (fr, id) in rows.iter().zip(file_ids.iter()) {
            let tracks = tracks_map.get(id).cloned().unwrap_or_default();
            files.push(fr.to_media_file(tracks)?);
        }

        Ok(files)
    }

    fn count_files(&self, filters: &FileFilters) -> Result<u64> {
        let conn = self.conn()?;
        let has_track_filter = filters.has_codec.is_some() || filters.has_language.is_some();
        let base = if has_track_filter {
            "SELECT COUNT(DISTINCT files.id) FROM files INNER JOIN tracks ON tracks.file_id = files.id WHERE 1=1"
        } else {
            "SELECT COUNT(DISTINCT files.id) FROM files WHERE 1=1"
        };
        let mut q = SqlQuery::new(base);

        if !filters.include_missing {
            q.condition(
                " AND files.status = {}",
                FileStatus::Active.as_str().to_string(),
            );
        }
        if let Some(container) = filters.container {
            q.condition(" AND files.container = {}", container.as_str().to_string());
        }
        if let Some(ref prefix) = filters.path_prefix {
            q.condition(
                " AND files.path LIKE {} ESCAPE '\\'",
                format!("{}%", escape_like(prefix)),
            );
        }
        if let Some(ref codec) = filters.has_codec {
            q.condition(" AND tracks.codec = {}", codec.clone());
        }
        if let Some(ref lang) = filters.has_language {
            q.condition(" AND tracks.language = {}", lang.clone());
        }

        let count: u64 = conn
            .query_row(&q.sql, q.param_refs().as_slice(), |row| row.get(0))
            .map_err(storage_err("failed to count files"))?;

        Ok(count)
    }

    fn mark_missing(&self, id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        conn.execute(
            "UPDATE files SET status = ?1, missing_since = ?2 WHERE id = ?3 AND status = ?4",
            params![
                FileStatus::Missing.as_str(),
                now,
                id.to_string(),
                FileStatus::Active.as_str()
            ],
        )
        .map_err(storage_err("failed to mark file missing"))?;
        Ok(())
    }

    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let path_str = new_path.to_string_lossy().to_string();
        let filename = filename_string(new_path);
        conn.execute(
            "UPDATE files SET status = ?1, missing_since = NULL, path = ?2, filename = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                FileStatus::Active.as_str(),
                path_str,
                filename,
                now,
                id.to_string()
            ],
        )
        .map_err(storage_err("failed to reactivate file"))?;
        Ok(())
    }

    fn rename_file_path(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let path_str = new_path.to_string_lossy().to_string();
        let filename = filename_string(new_path);
        conn.execute(
            "UPDATE files SET path = ?1, filename = ?2, updated_at = ?3 WHERE id = ?4",
            params![path_str, filename, now, id.to_string()],
        )
        .map_err(storage_err("failed to rename file path"))?;
        Ok(())
    }

    fn purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn()?;
        let cutoff = format_datetime(&older_than);
        conn.execute(
            "DELETE FROM file_transitions WHERE file_id IN (SELECT id FROM files WHERE status = ?1 AND missing_since < ?2)",
            params![FileStatus::Missing.as_str(), cutoff],
        )
        .map_err(storage_err("failed to purge transitions for missing files"))?;
        let deleted = conn
            .execute(
                "DELETE FROM files WHERE status = ?1 AND missing_since < ?2",
                params![FileStatus::Missing.as_str(), cutoff],
            )
            .map_err(storage_err("failed to purge missing files"))?;
        Ok(deleted as u64)
    }

    fn reconcile_discovered_files(
        &self,
        discovered: &[DiscoveredFile],
        scanned_dirs: &[PathBuf],
    ) -> Result<ReconcileResult> {
        let session = self.begin_scan_session(scanned_dirs)?;
        let mut result = ReconcileResult::default();

        for df in discovered {
            let decision = self.ingest_discovered_file(session, df)?;
            match &decision {
                IngestDecision::New { .. } => result.new_files += 1,
                IngestDecision::Unchanged { .. } => result.unchanged += 1,
                IngestDecision::ExternallyChanged { .. } => result.external_changes += 1,
                IngestDecision::Moved { .. } => result.moved += 1,
                IngestDecision::Duplicate { .. } => {
                    // Duplicate paths in the input list are dropped silently to
                    // preserve today's `HashSet`-based dedup behavior.
                }
            }
            if let Some(p) = decision.needs_introspection_path(&df.path) {
                result.needs_introspection.push(p);
            }
        }

        let finish = self.finish_scan_session(session)?;
        result.missing = finish.missing;
        // Promoted moves: each was counted as New during ingestion, so decrement
        // new_files and increment moved to reflect the retroactive reclassification.
        result.new_files = result.new_files.saturating_sub(finish.promoted_moves);
        result.moved += finish.promoted_moves;
        Ok(result)
    }

    fn update_expected_hash(&self, id: &Uuid, hash: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
            params![hash, id.to_string()],
        )
        .map_err(storage_err("failed to update expected_hash"))?;
        Ok(())
    }

    fn set_file_status(&self, id: &Uuid, status: FileStatus) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        conn.execute(
            "UPDATE files SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now, id.to_string()],
        )
        .map_err(storage_err("failed to set file status"))?;
        Ok(())
    }

    fn record_post_execution(
        &self,
        new_path: Option<&Path>,
        new_expected_hash: Option<&str>,
        transition: &FileTransition,
    ) -> Result<()> {
        debug_assert!(
            new_expected_hash.is_none_or(|h| !h.is_empty()),
            "record_post_execution: empty hash passed as Some — use None instead"
        );

        let id_str = transition.file_id.to_string();
        let mut conn = self.conn()?;
        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin post-execution transaction"))?;

        if let Some(new_path) = new_path {
            let now = format_datetime(&Utc::now());
            let path_str = new_path.to_string_lossy().to_string();
            let filename = filename_string(new_path);
            tx.execute(
                "UPDATE files SET path = ?1, filename = ?2, updated_at = ?3 WHERE id = ?4",
                params![path_str, filename, now, &id_str],
            )
            .map_err(storage_err("failed to rename file path in bundle"))?;
        }

        super::file_transition_storage::insert_full_transition_in_tx(&tx, transition)?;

        if let Some(hash) = new_expected_hash {
            tx.execute(
                "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
                params![hash, &id_str],
            )
            .map_err(storage_err("failed to update expected_hash in bundle"))?;
        }

        // Symmetric with `handle_file_introspected`'s cleanup: a successful
        // bundle means the file at `transition.path` is no longer "bad".
        super::bad_file_storage::delete_bad_file_by_path_in_tx(&tx, &transition.path)?;

        tx.commit()
            .map_err(storage_err("failed to commit post-execution bundle"))?;
        Ok(())
    }

    fn predecessor_of(&self, successor_id: &Uuid) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, expected_hash, status, \
                 container, duration, bitrate, crop_left, crop_top, crop_right, crop_bottom, \
                 crop_detected_at, crop_settings_fingerprint, tags, plugin_metadata, introspected_at \
                 FROM files WHERE superseded_by = ?1 LIMIT 1",
                params![successor_id.to_string()],
                row_to_file,
            )
            .optional()
            .map_err(storage_err(&format!(
                "failed to query predecessor for {successor_id}"
            )))?;

        match file_row {
            Some(fr) => {
                let id = super::parse_uuid(&fr.id)?;
                let tracks = self.load_tracks(&conn, &id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
    }

    fn predecessor_id_of(&self, successor_id: &Uuid) -> Result<Option<Uuid>> {
        let conn = self.conn()?;
        let id_str: Option<String> = conn
            .query_row(
                "SELECT id FROM files WHERE superseded_by = ?1 LIMIT 1",
                params![successor_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err(&format!(
                "failed to query predecessor id for {successor_id}"
            )))?;

        id_str.map(|s| super::parse_uuid(&s)).transpose()
    }

    fn mark_missing_paths(
        &self,
        discovered_paths: &[PathBuf],
        scanned_dirs: &[PathBuf],
    ) -> Result<u32> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());

        let mut stmt = conn
            .prepare("SELECT id, path FROM files WHERE status = 'active' AND path IS NOT NULL")
            .map_err(storage_err("failed to prepare missing-path check"))?;
        let active_files: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage_err("failed to query active files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect active files"))?;

        let discovered_set: HashSet<String> = discovered_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut marked = 0u32;
        for (id, path) in &active_files {
            let path_obj = std::path::Path::new(path);
            let under_scanned = scanned_dirs.iter().any(|dir| path_obj.starts_with(dir));
            if under_scanned && !discovered_set.contains(path.as_str()) {
                conn.execute(
                    "UPDATE files SET status = 'missing', missing_since = ?1 \
                     WHERE id = ?2 AND status = 'active'",
                    params![&now, id],
                )
                .map_err(storage_err("failed to mark missing"))?;
                marked += 1;
            }
        }
        Ok(marked)
    }

    fn begin_scan_session(
        &self,
        roots: &[PathBuf],
    ) -> voom_domain::errors::Result<voom_domain::transition::ScanSessionId> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let roots_json = serde_json::to_string(
            &roots
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
        )
        .map_err(other_storage_err("failed to serialize scan roots"))?;
        let id = voom_domain::transition::ScanSessionId::new();

        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin scan_session transaction"))?;
        // SELECT the IDs before the UPDATE so the warn! log preserves them for
        // forensic trace (after the UPDATE, those rows no longer match
        // status='in_progress').
        let abandoned_ids: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM scan_sessions WHERE status = 'in_progress'")
                .map_err(storage_err("failed to prepare prior-session lookup"))?;
            stmt.query_map([], |row| row.get::<_, String>(0))
                .map_err(storage_err("failed to query prior in_progress sessions"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err(
                    "failed to collect prior in_progress session IDs",
                ))?
        };
        if !abandoned_ids.is_empty() {
            let abandoned = abandoned_ids.len();
            tracing::warn!(
                abandoned,
                cancelled_ids = ?abandoned_ids,
                "auto-cancelled stale in_progress scan session(s) at begin",
            );
            tx.execute(
                "UPDATE scan_sessions SET status = 'cancelled', finished_at = ?1 \
                 WHERE status = 'in_progress'",
                params![now],
            )
            .map_err(storage_err("failed to auto-abandon prior scan sessions"))?;
        }
        tx.execute(
            "INSERT INTO scan_sessions (id, roots_json, status, started_at, last_heartbeat_at) \
             VALUES (?1, ?2, 'in_progress', ?3, ?3)",
            params![id.to_string(), roots_json, now],
        )
        .map_err(storage_err("failed to insert scan session"))?;
        tx.commit()
            .map_err(storage_err("failed to commit begin_scan_session"))?;
        Ok(id)
    }

    fn ingest_discovered_file(
        &self,
        session: voom_domain::transition::ScanSessionId,
        file: &voom_domain::transition::DiscoveredFile,
    ) -> voom_domain::errors::Result<voom_domain::transition::IngestDecision> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let session_str = session.to_string();
        let path_str = file.path.to_string_lossy().to_string();
        let filename = filename_string(&file.path);

        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin ingest transaction"))?;

        // Session must be in_progress
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM scan_sessions WHERE id = ?1",
                params![&session_str],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err("failed to look up scan session"))?;
        match status.as_deref() {
            Some("in_progress") => {}
            Some(other) => {
                return Err(voom_domain::errors::VoomError::Storage {
                    kind: voom_domain::errors::StorageErrorKind::Other,
                    message: format!(
                        "scan session {session_str} is not in_progress (status: {other})"
                    ),
                });
            }
            None => {
                return Err(voom_domain::errors::VoomError::Storage {
                    kind: voom_domain::errors::StorageErrorKind::NotFound,
                    message: format!("unknown scan session {session_str}"),
                });
            }
        }

        // Look up existing row by path
        let existing: Option<(String, Option<String>, i64, Option<String>)> = tx
            .query_row(
                "SELECT id, expected_hash, size, last_seen_session_id \
                 FROM files WHERE path = ?1",
                params![&path_str],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err("failed to look up existing file"))?;

        if let Some((existing_id, expected_hash, existing_size, last_seen)) = existing {
            // Idempotency: if this path was already touched by this session,
            // return Duplicate immediately. See spec §6.2 step 3.
            if last_seen.as_deref() == Some(session_str.as_str()) {
                let file_uuid = super::parse_uuid(&existing_id)?;
                tx.commit()
                    .map_err(storage_err("failed to commit duplicate ingest"))?;
                return Ok(IngestDecision::Duplicate { file_id: file_uuid });
            }

            // Check for content match (Unchanged) vs hash mismatch (ExternallyChanged).
            let hash_matches = expected_hash
                .as_ref()
                .is_none_or(|eh| eh == &file.content_hash);

            if hash_matches {
                tx.execute(
                    "UPDATE files SET size = ?1, content_hash = ?2, \
                     status = 'active', missing_since = NULL, \
                     last_seen_session_id = ?3, \
                     updated_at = ?4 WHERE id = ?5",
                    params![
                        file.size as i64,
                        &file.content_hash,
                        &session_str,
                        &now,
                        &existing_id
                    ],
                )
                .map_err(storage_err("failed to update unchanged file"))?;

                if expected_hash.is_none() {
                    tx.execute(
                        "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
                        params![&file.content_hash, &existing_id],
                    )
                    .map_err(storage_err("failed to backfill expected_hash"))?;
                }

                tx.commit()
                    .map_err(storage_err("failed to commit unchanged ingest"))?;
                let file_uuid = super::parse_uuid(&existing_id)?;
                return Ok(IngestDecision::Unchanged { file_id: file_uuid });
            }

            // Hash mismatch: external change. Mark old missing+superseded;
            // insert a new row; emit External + Discovery transitions.
            let old_uuid = super::parse_uuid(&existing_id)?;
            let new_id = Uuid::new_v4();

            let ext_transition = FileTransition::new(
                old_uuid,
                file.path.clone(),
                file.content_hash.clone(),
                file.size,
                TransitionSource::External,
            )
            .with_from(expected_hash.clone(), Some(existing_size as u64));
            insert_transition_in_tx(&tx, &ext_transition, &now)?;

            tx.execute(
                "UPDATE files SET path = NULL, status = 'missing', \
                 missing_since = ?1, superseded_by = ?2 WHERE id = ?3",
                params![&now, new_id.to_string(), &existing_id],
            )
            .map_err(storage_err("failed to clear old file for external change"))?;
            tx.execute(
                "INSERT INTO files \
                 (id, path, filename, size, content_hash, \
                  expected_hash, status, container, duration, \
                  tags, plugin_metadata, introspected_at, \
                  created_at, updated_at, last_seen_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', \
                         'other', 0.0, '{}', '{}', ?7, ?7, ?7, ?8)",
                params![
                    new_id.to_string(),
                    &path_str,
                    filename,
                    file.size as i64,
                    &file.content_hash,
                    &file.content_hash,
                    &now,
                    &session_str,
                ],
            )
            .map_err(storage_err("failed to insert new file for external change"))?;

            let disc_transition = FileTransition::new(
                new_id,
                file.path.clone(),
                file.content_hash.clone(),
                file.size,
                TransitionSource::Discovery,
            );
            insert_transition_in_tx(&tx, &disc_transition, &now)?;

            tx.commit()
                .map_err(storage_err("failed to commit external-change ingest"))?;
            return Ok(IngestDecision::ExternallyChanged {
                file_id: new_id,
                superseded: old_uuid,
            });
        }

        // No row at this path. Check for move: a missing file with matching
        // expected_hash. The status='missing' filter is what prevents
        // double-consumption within a session — once we reactivate a row,
        // it's no longer 'missing' and won't match again.
        //
        // Constrain move detection to the current session's scanned roots —
        // same scoping as build_missing_hash_index on main. Without this, a
        // hash collision between roots would falsely promote a cross-root missing
        // file to Moved.
        let session_roots: Vec<PathBuf> = {
            let roots_json: String = tx
                .query_row(
                    "SELECT roots_json FROM scan_sessions WHERE id = ?1",
                    params![&session_str],
                    |row| row.get(0),
                )
                .map_err(storage_err("failed to load session roots for move lookup"))?;
            let roots: Vec<String> = serde_json::from_str(&roots_json).map_err(|e| {
                voom_domain::errors::VoomError::Other(
                    format!("failed to parse roots_json: {e}").into(),
                )
            })?;
            roots.into_iter().map(PathBuf::from).collect()
        };

        let move_match: Option<(String, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, path FROM files \
                     WHERE expected_hash = ?1 AND status = 'missing' AND path IS NOT NULL",
                )
                .map_err(storage_err("failed to prepare move-candidate select"))?;
            let candidates: Vec<(String, String)> = stmt
                .query_map(params![&file.content_hash], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(storage_err("failed to query move candidates"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err("failed to collect move candidates"))?;
            candidates.into_iter().find(|(_, path)| {
                let p = Path::new(path);
                session_roots.iter().any(|r| p.starts_with(r))
            })
        };

        if let Some((missing_id, prior_path)) = move_match {
            tx.execute(
                "UPDATE files SET path = ?1, filename = ?2, \
                 size = ?3, content_hash = ?4, \
                 status = 'active', missing_since = NULL, \
                 last_seen_session_id = ?5, \
                 updated_at = ?6 WHERE id = ?7",
                params![
                    &path_str,
                    filename,
                    file.size as i64,
                    &file.content_hash,
                    &session_str,
                    &now,
                    &missing_id,
                ],
            )
            .map_err(storage_err("failed to reactivate moved file"))?;

            let file_uuid = super::parse_uuid(&missing_id)?;
            let move_transition = voom_domain::transition::FileTransition::new(
                file_uuid,
                file.path.clone(),
                file.content_hash.clone(),
                file.size,
                voom_domain::transition::TransitionSource::Discovery,
            )
            .with_from_path(PathBuf::from(prior_path.clone()))
            .with_detail("detected_move");
            insert_transition_in_tx(&tx, &move_transition, &now)?;

            tx.commit()
                .map_err(storage_err("failed to commit moved ingest"))?;
            return Ok(IngestDecision::Moved {
                file_id: file_uuid,
                from_path: PathBuf::from(prior_path),
            });
        }

        // Truly new file
        let new_id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO files \
             (id, path, filename, size, content_hash, \
              expected_hash, status, container, duration, \
              tags, plugin_metadata, introspected_at, \
              created_at, updated_at, last_seen_session_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', \
                     'other', 0.0, '{}', '{}', ?7, ?7, ?7, ?8)",
            params![
                new_id.to_string(),
                &path_str,
                filename,
                file.size as i64,
                &file.content_hash,
                &file.content_hash,
                &now,
                &session_str,
            ],
        )
        .map_err(storage_err("failed to insert new file in ingest"))?;

        let disc_transition = FileTransition::new(
            new_id,
            file.path.clone(),
            file.content_hash.clone(),
            file.size,
            TransitionSource::Discovery,
        );
        insert_transition_in_tx(&tx, &disc_transition, &now)?;

        tx.commit()
            .map_err(storage_err("failed to commit ingest transaction"))?;

        Ok(IngestDecision::New {
            file_id: new_id,
            needs_introspection: true,
        })
    }

    fn finish_scan_session(
        &self,
        session: voom_domain::transition::ScanSessionId,
    ) -> voom_domain::errors::Result<ScanFinishOutcome> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let session_str = session.to_string();

        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin finish_scan_session tx"))?;

        let row: Option<(String, String, String)> = tx
            .query_row(
                "SELECT status, roots_json, started_at FROM scan_sessions WHERE id = ?1",
                params![&session_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(storage_err("failed to load scan session"))?;

        let (status, roots_json, session_started_at) = match row {
            Some(v) => v,
            None => {
                return Err(voom_domain::errors::VoomError::Storage {
                    kind: voom_domain::errors::StorageErrorKind::Other,
                    message: format!("unknown scan session {session_str}"),
                });
            }
        };

        if status != "in_progress" {
            return Err(voom_domain::errors::VoomError::Storage {
                kind: voom_domain::errors::StorageErrorKind::Other,
                message: format!(
                    "scan session {session_str} is not in_progress (status: {status})",
                ),
            });
        }

        let roots: Vec<String> = serde_json::from_str(&roots_json)
            .map_err(other_storage_err("failed to parse roots_json"))?;
        let roots: Vec<PathBuf> = roots.into_iter().map(PathBuf::from).collect();

        // ---- MOVE PROMOTION ----
        // Files ingested as New this session may actually be moves from an active
        // file that's about to be marked missing. Detect these by matching
        // content_hash (New-this-session row) against expected_hash (missing candidate).
        // "New this session" == last_seen_session_id == this session AND created_at >= started_at.

        struct MissingCandidate {
            id: String,
            path: String,
            expected_hash: String,
        }

        let candidates_for_move: Vec<MissingCandidate> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, path, expected_hash FROM files \
                     WHERE status = 'active' AND path IS NOT NULL \
                       AND expected_hash IS NOT NULL \
                       AND (last_seen_session_id IS NULL OR last_seen_session_id != ?1)",
                )
                .map_err(storage_err("failed to prepare move-candidate select"))?;
            stmt.query_map(params![&session_str], |row| {
                Ok(MissingCandidate {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    expected_hash: row.get(2)?,
                })
            })
            .map_err(storage_err("failed to query move candidates"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect move candidates"))?
            .into_iter()
            .filter(|c| {
                let p = Path::new(&c.path);
                roots.iter().any(|r| p.starts_with(r))
            })
            .collect()
        };

        let mut promoted_moves = 0u32;

        for candidate in &candidates_for_move {
            // Find a New-this-session row with content_hash matching this candidate's
            // expected_hash. "New this session" == last_seen_session_id == this session
            // AND created_at >= session started_at.
            // Exclude rows that are part of a supersession chain — these are
            // ExternallyChanged replacement rows (their id appears as superseded_by
            // on the old row) and must not be hijacked as move targets.
            let new_match: Option<(String, String, i64)> = tx
                .query_row(
                    "SELECT id, path, size FROM files \
                     WHERE last_seen_session_id = ?1 \
                       AND content_hash = ?2 \
                       AND created_at >= ?3 \
                       AND path IS NOT NULL \
                       AND id != ?4 \
                       AND id NOT IN (SELECT superseded_by FROM files \
                                      WHERE superseded_by IS NOT NULL) \
                     LIMIT 1",
                    params![
                        &session_str,
                        &candidate.expected_hash,
                        &session_started_at,
                        &candidate.id,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(storage_err("failed to look up move-promotion target"))?;

            let Some((new_id, new_path, new_size)) = new_match else {
                continue;
            };

            // Delete the New row's transitions so the move transition is the only record.
            tx.execute(
                "DELETE FROM file_transitions WHERE file_id = ?1",
                params![&new_id],
            )
            .map_err(storage_err("failed to delete New row's transitions"))?;

            // Delete the New row itself.
            tx.execute("DELETE FROM files WHERE id = ?1", params![&new_id])
                .map_err(storage_err("failed to delete New row for move promotion"))?;

            // Update the candidate row to reflect the new path and keep it active.
            let new_filename = Path::new(&new_path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            tx.execute(
                "UPDATE files SET path = ?1, filename = ?2, size = ?3, \
                 content_hash = ?4, \
                 status = 'active', missing_since = NULL, \
                 last_seen_session_id = ?5, updated_at = ?6 \
                 WHERE id = ?7",
                params![
                    &new_path,
                    new_filename,
                    new_size,
                    &candidate.expected_hash,
                    &session_str,
                    &now,
                    &candidate.id,
                ],
            )
            .map_err(storage_err("failed to promote candidate to moved"))?;

            // Record a Discovery transition with from_path set to signal the move.
            let file_uuid = super::parse_uuid(&candidate.id)?;
            let move_tx = FileTransition::new(
                file_uuid,
                PathBuf::from(&new_path),
                candidate.expected_hash.clone(),
                new_size as u64,
                TransitionSource::Discovery,
            )
            .with_from_path(PathBuf::from(&candidate.path))
            .with_detail("detected_move");
            insert_transition_in_tx(&tx, &move_tx, &now)?;

            promoted_moves += 1;
        }

        // ---- MISSING PASS ----
        // Path-prefix filtering in Rust mirrors the historical batch-reconcile
        // missing pass (status='active' AND path under any scanned root AND
        // not seen by this session). The `AND status='active'` clause on the
        // UPDATE below guards against a race between the SELECT and UPDATE.
        // Promoted candidates now have last_seen_session_id == this session,
        // so the query below won't touch them.
        let candidates: Vec<(String, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id, path FROM files \
                     WHERE status = 'active' AND path IS NOT NULL \
                       AND (last_seen_session_id IS NULL OR last_seen_session_id != ?1)",
                )
                .map_err(storage_err("failed to prepare missing-scan select"))?;
            stmt.query_map(params![&session_str], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err("failed to query missing candidates"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect missing candidates"))?
        };

        let mut missing = 0u32;
        for (id, path) in candidates {
            let p = Path::new(&path);
            if !roots.iter().any(|r| p.starts_with(r)) {
                continue;
            }
            tx.execute(
                "UPDATE files SET status = 'missing', missing_since = ?1, updated_at = ?1 \
                 WHERE id = ?2 AND status = 'active'",
                params![&now, &id],
            )
            .map_err(storage_err("failed to mark file missing in finish"))?;
            missing += 1;
        }

        tx.execute(
            "UPDATE scan_sessions SET status = 'completed', finished_at = ?1 WHERE id = ?2",
            params![&now, &session_str],
        )
        .map_err(storage_err("failed to mark session completed"))?;

        tx.commit()
            .map_err(storage_err("failed to commit finish_scan_session"))?;
        Ok(ScanFinishOutcome::new(missing, promoted_moves))
    }

    fn cancel_scan_session(
        &self,
        session: voom_domain::transition::ScanSessionId,
    ) -> voom_domain::errors::Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        conn.execute(
            "UPDATE scan_sessions \
             SET status = 'cancelled', finished_at = ?1 \
             WHERE id = ?2 AND status = 'in_progress'",
            params![now, session.to_string()],
        )
        .map_err(storage_err("failed to cancel scan session"))?;
        Ok(())
    }
}

fn insert_transition_in_tx(
    tx: &rusqlite::Transaction<'_>,
    t: &FileTransition,
    now: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO file_transitions \
         (id, file_id, path, from_path, from_hash, to_hash, from_size, to_size, \
          source, source_detail, plan_id, \
          duration_ms, actions_taken, tracks_modified, outcome, \
          policy_name, phase_name, metadata_snapshot, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, \
                 ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            t.id.to_string(),
            t.file_id.to_string(),
            t.path.to_string_lossy().to_string(),
            t.from_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            t.from_hash.as_deref(),
            t.to_hash,
            t.from_size.map(|v| v as i64),
            t.to_size as i64,
            t.source.as_str(),
            t.source_detail.as_deref(),
            t.plan_id.map(|id| id.to_string()),
            t.duration_ms.map(|v| v as i64),
            t.actions_taken.map(i64::from),
            t.tracks_modified.map(i64::from),
            t.outcome.map(|o| o.as_str()),
            t.policy_name.as_deref(),
            t.phase_name.as_deref(),
            t.metadata_snapshot.as_ref().and_then(|s| {
                s.to_json()
                    .map_err(
                        |e| tracing::warn!(error = %e, "failed to serialize metadata_snapshot"),
                    )
                    .ok()
            }),
            now,
        ],
    )
    .map_err(storage_err("failed to insert transition"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::MediaFile;
    use voom_domain::storage::FileStorage;
    use voom_domain::transition::FileStatus;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    fn active_file(path: &str) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path));
        file.content_hash = Some("abc123".to_string());
        file.expected_hash = Some("abc123".to_string());
        file
    }

    #[test]
    fn list_files_rejects_unknown_file_status() {
        let store = test_store();
        let file = active_file("/media/bad-status.mkv");
        let file_id = file.id;
        store.upsert_file(&file).unwrap();

        let conn = store.conn().unwrap();
        conn.execute(
            "UPDATE files SET status = ?1 WHERE id = ?2",
            params!["mystery", file_id.to_string()],
        )
        .unwrap();

        let mut filters = voom_domain::storage::FileFilters::default();
        filters.include_missing = true;
        let err = store.list_files(&filters).unwrap_err();
        assert!(err.to_string().contains("unknown file status"));
    }

    #[test]
    fn mark_missing_paths_basic() {
        let store = test_store();
        let file_a = active_file("/media/a.mkv");
        let file_b = active_file("/media/b.mkv");
        store.upsert_file(&file_a).unwrap();
        store.upsert_file(&file_b).unwrap();

        // Only a.mkv is in discovered set — b.mkv should be marked missing
        let discovered = vec![PathBuf::from("/media/a.mkv")];
        let scanned = vec![PathBuf::from("/media")];
        let count = store.mark_missing_paths(&discovered, &scanned).unwrap();

        assert_eq!(count, 1);
        let stored_b = store
            .file_by_path(std::path::Path::new("/media/b.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_b.status, FileStatus::Missing);
        let stored_a = store
            .file_by_path(std::path::Path::new("/media/a.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_a.status, FileStatus::Active);
    }

    #[test]
    fn reconcile_move_detection_scoped_to_scan_roots() {
        let store = test_store();

        // File A: in /tv, marked missing with a known hash
        let mut file_a = MediaFile::new(PathBuf::from("/tv/episode.mkv"));
        file_a.expected_hash = Some("abc".to_string());
        file_a.content_hash = Some("abc".to_string());
        store.upsert_file(&file_a).unwrap();
        let stored_a = store
            .file_by_path(Path::new("/tv/episode.mkv"))
            .unwrap()
            .unwrap();
        let file_a_id = stored_a.id;
        store.mark_missing(&file_a_id).unwrap();

        // File B: already active in /movies (different hash)
        let mut file_b = MediaFile::new(PathBuf::from("/movies/movie.mkv"));
        file_b.expected_hash = Some("def".to_string());
        file_b.content_hash = Some("def".to_string());
        store.upsert_file(&file_b).unwrap();

        // Discover /movies/new.mkv with the same hash as /tv/episode.mkv
        // scanning only /movies/ — move detection must NOT steal /tv file's identity
        let discovered = vec![voom_domain::transition::DiscoveredFile::new(
            PathBuf::from("/movies/new.mkv"),
            1000,
            "abc".to_string(),
        )];
        let scanned = vec![PathBuf::from("/movies")];
        let result = store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();

        // /movies/new.mkv should be a brand-new file, not a move of /tv/episode.mkv
        assert_eq!(result.new_files, 1, "expected 1 new file");
        assert_eq!(result.moved, 0, "expected no moves");

        // /tv/episode.mkv must still be missing with its original ID
        let still_missing = store
            .file_by_path(Path::new("/tv/episode.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(
            still_missing.status,
            FileStatus::Missing,
            "/tv/episode.mkv should still be missing"
        );
        assert_eq!(
            still_missing.id, file_a_id,
            "/tv/episode.mkv should keep its original UUID"
        );

        // /movies/new.mkv must have a different UUID from /tv/episode.mkv
        let new_file = store
            .file_by_path(Path::new("/movies/new.mkv"))
            .unwrap()
            .unwrap();
        assert_ne!(
            new_file.id, file_a_id,
            "/movies/new.mkv must have a new UUID, not stolen from /tv/episode.mkv"
        );
    }

    #[test]
    fn reconcile_move_records_from_path_on_move_transition() {
        use voom_domain::storage::FileTransitionStorage;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();

        // File begins life at /media/old/film.mkv.
        let mut file = MediaFile::new(PathBuf::from("/media/old/film.mkv"));
        file.content_hash = Some("hash".to_string());
        store.upsert_file(&file).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/old/film.mkv"))
            .unwrap()
            .unwrap()
            .id;
        // upsert_file doesn't persist expected_hash; set it so the missing-by-hash
        // index can pick the row up as a move candidate.
        store.update_expected_hash(&original_id, "hash").unwrap();
        store.mark_missing(&original_id).unwrap();

        // Same hash discovered at a new path within the same scan root.
        let discovered = vec![DiscoveredFile::new(
            PathBuf::from("/media/new/film.mkv"),
            1_000,
            "hash".to_string(),
        )];
        let scanned = vec![PathBuf::from("/media")];
        let result = store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();
        assert_eq!(result.moved, 1);

        // The move transition must reference the prior path so old-path lookups work.
        let by_old_path = store
            .transitions_for_path(Path::new("/media/old/film.mkv"))
            .unwrap();
        assert_eq!(
            by_old_path.len(),
            1,
            "old path should resolve via from_path"
        );
        assert_eq!(
            by_old_path[0].from_path.as_deref(),
            Some(Path::new("/media/old/film.mkv"))
        );
        assert_eq!(by_old_path[0].path, Path::new("/media/new/film.mkv"));

        // And the new path also resolves.
        let by_new_path = store
            .transitions_for_path(Path::new("/media/new/film.mkv"))
            .unwrap();
        assert_eq!(by_new_path.len(), 1);
        assert_eq!(by_new_path[0].id, by_old_path[0].id);
    }

    #[test]
    fn predecessor_of_returns_none_when_no_predecessor() {
        let store = test_store();
        let file = active_file("/media/movie.mkv");
        store.upsert_file(&file).unwrap();

        let result = store.predecessor_of(&file.id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn predecessor_of_follows_superseded_by_link() {
        let store = test_store();

        // Create old file, then manually set superseded_by
        let old_file = active_file("/media/old.mkv");
        store.upsert_file(&old_file).unwrap();
        let new_file = active_file("/media/movie.mkv");
        store.upsert_file(&new_file).unwrap();

        // Manually set the link (reconciliation will do this in Task 3)
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET superseded_by = ?1 WHERE id = ?2",
                rusqlite::params![new_file.id.to_string(), old_file.id.to_string()],
            )
            .unwrap();
        }

        let predecessor = store.predecessor_of(&new_file.id).unwrap();
        assert!(predecessor.is_some(), "should find predecessor");
        assert_eq!(predecessor.unwrap().id, old_file.id);
    }

    #[test]
    fn reconcile_external_change_sets_superseded_by() {
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/movie.mkv"));
        file.content_hash = Some("original_hash".to_string());
        store.upsert_file(&file).unwrap();
        let old_id = store
            .file_by_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap()
            .id;
        // upsert_file doesn't persist expected_hash; set it explicitly so reconciliation
        // detects a hash mismatch and treats this as an external modification.
        store
            .update_expected_hash(&old_id, "original_hash")
            .unwrap();

        let discovered = vec![DiscoveredFile::new(
            PathBuf::from("/media/movie.mkv"),
            2000,
            "new_hash".to_string(),
        )];
        let scanned = vec![PathBuf::from("/media")];
        let result = store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();

        assert_eq!(result.external_changes, 1);

        let new_file = store
            .file_by_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_ne!(new_file.id, old_id, "new file should have a different UUID");

        let predecessor = store.predecessor_of(&new_file.id).unwrap();
        assert!(predecessor.is_some(), "new file should have a predecessor");
        assert_eq!(
            predecessor.unwrap().id,
            old_id,
            "predecessor should be the old file"
        );
    }

    #[test]
    fn reconcile_successive_external_changes_build_chain() {
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();

        let mut file = MediaFile::new(PathBuf::from("/media/movie.mkv"));
        file.content_hash = Some("hash_v1".to_string());
        store.upsert_file(&file).unwrap();
        let id_v1 = store
            .file_by_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap()
            .id;
        // upsert_file doesn't persist expected_hash; set it explicitly.
        store.update_expected_hash(&id_v1, "hash_v1").unwrap();

        // First external modification: v1 -> v2
        let discovered = vec![DiscoveredFile::new(
            PathBuf::from("/media/movie.mkv"),
            2000,
            "hash_v2".to_string(),
        )];
        let scanned = vec![PathBuf::from("/media")];
        store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();
        let id_v2 = store
            .file_by_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap()
            .id;

        // Second external modification: v2 -> v3
        let discovered = vec![DiscoveredFile::new(
            PathBuf::from("/media/movie.mkv"),
            3000,
            "hash_v3".to_string(),
        )];
        store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();
        let file_v3 = store
            .file_by_path(std::path::Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();

        // Walk the chain backward: v3 -> v2 -> v1
        let pred_v2 = store.predecessor_of(&file_v3.id).unwrap();
        assert!(pred_v2.is_some(), "v3 should have predecessor v2");
        assert_eq!(pred_v2.as_ref().unwrap().id, id_v2);

        let pred_v1 = store.predecessor_of(&id_v2).unwrap();
        assert!(pred_v1.is_some(), "v2 should have predecessor v1");
        assert_eq!(pred_v1.as_ref().unwrap().id, id_v1);

        let pred_none = store.predecessor_of(&id_v1).unwrap();
        assert!(pred_none.is_none(), "v1 should have no predecessor");
    }

    #[test]
    fn predecessor_id_of_returns_just_uuid() {
        let store = test_store();

        let old_file = active_file("/media/old.mkv");
        store.upsert_file(&old_file).unwrap();
        let new_file = active_file("/media/movie.mkv");
        store.upsert_file(&new_file).unwrap();

        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET superseded_by = ?1 WHERE id = ?2",
                rusqlite::params![new_file.id.to_string(), old_file.id.to_string()],
            )
            .unwrap();
        }

        let pred_id = store.predecessor_id_of(&new_file.id).unwrap();
        assert_eq!(pred_id, Some(old_file.id));

        let no_pred = store.predecessor_id_of(&old_file.id).unwrap();
        assert!(no_pred.is_none());
    }

    #[test]
    fn rename_file_path_preserves_id_and_data() {
        let store = test_store();
        let file = active_file("/media/movie.mp4");
        store.upsert_file(&file).unwrap();

        let stored_before = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap();
        let original_id = stored_before.id;

        store
            .rename_file_path(&original_id, Path::new("/media/movie.mkv"))
            .unwrap();

        // Old path no longer maps to a row
        assert!(
            store
                .file_by_path(Path::new("/media/movie.mp4"))
                .unwrap()
                .is_none()
        );

        // New path maps to the same row (same id, same hash, status untouched)
        let stored_after = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_after.id, original_id);
        assert_eq!(stored_after.content_hash.as_deref(), Some("abc123"));
        assert_eq!(stored_after.status, FileStatus::Active);

        // Filename was derived from the new path
        let conn = store.conn().unwrap();
        let filename: String = conn
            .query_row(
                "SELECT filename FROM files WHERE id = ?1",
                rusqlite::params![original_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(filename, "movie.mkv");
    }

    #[test]
    fn rename_then_upsert_updates_existing_row_in_place() {
        // Simulates the ConvertContainer flow: existing mp4 row is renamed
        // to the .mkv path, then re-introspection upserts a freshly-introspected
        // MediaFile with a NEW UUID at that path. The path-based ON CONFLICT
        // must merge into the existing row rather than insert a duplicate.
        let store = test_store();
        let original = active_file("/media/movie.mp4");
        store.upsert_file(&original).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap()
            .id;

        store
            .rename_file_path(&original_id, Path::new("/media/movie.mkv"))
            .unwrap();

        // Simulated re-introspection: brand-new MediaFile (new UUID) at the new path.
        let mut reintrospected = MediaFile::new(PathBuf::from("/media/movie.mkv"));
        reintrospected.content_hash = Some("def456".to_string());
        reintrospected.container = voom_domain::media::Container::Mkv;
        assert_ne!(reintrospected.id, original_id, "test setup: new uuid");

        store.upsert_file(&reintrospected).unwrap();

        // Exactly one row, surviving id is the original, content reflects the upsert.
        let count = store
            .count_files(&voom_domain::storage::FileFilters::default())
            .unwrap();
        assert_eq!(count, 1, "rename + upsert must not duplicate the row");

        let surviving = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(
            surviving.id, original_id,
            "lineage must be preserved via path-based upsert merge"
        );
        assert_eq!(surviving.content_hash.as_deref(), Some("def456"));
        assert_eq!(surviving.container, voom_domain::media::Container::Mkv);
    }

    #[test]
    fn mark_missing_paths_scoped_to_scanned_dirs() {
        let store = test_store();
        let movies_file = active_file("/movies/film.mkv");
        let tv_file = active_file("/tv/show.mkv");
        store.upsert_file(&movies_file).unwrap();
        store.upsert_file(&tv_file).unwrap();

        // Scan only /movies with nothing discovered — only /movies file should be marked
        let scanned = vec![PathBuf::from("/movies")];
        let count = store.mark_missing_paths(&[], &scanned).unwrap();

        assert_eq!(count, 1);
        let stored_movie = store
            .file_by_path(std::path::Path::new("/movies/film.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_movie.status, FileStatus::Missing);
        let stored_tv = store
            .file_by_path(std::path::Path::new("/tv/show.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_tv.status, FileStatus::Active);
    }

    // --- Direct unit tests below ---

    #[test]
    fn file_unknown_id_returns_none() {
        let store = test_store();
        let missing = store.file(&Uuid::new_v4()).unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn file_by_path_unknown_returns_none() {
        let store = test_store();
        let missing = store
            .file_by_path(Path::new("/nowhere/nothing.mkv"))
            .unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn list_files_empty_db() {
        let store = test_store();
        let files = store.list_files(&FileFilters::default()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn count_files_matches_list_files() {
        let store = test_store();
        for i in 0..4 {
            let mut f = MediaFile::new(PathBuf::from(format!("/media/f{i}.mkv")));
            f.content_hash = Some(format!("hash{i}"));
            store.upsert_file(&f).unwrap();
        }
        let filters = FileFilters::default();
        let count = store.count_files(&filters).unwrap();
        let list = store.list_files(&filters).unwrap();
        assert_eq!(count, list.len() as u64);
        assert_eq!(count, 4);
    }

    #[test]
    fn list_files_path_prefix_filter() {
        let store = test_store();
        let a = active_file("/movies/a.mkv");
        let b = active_file("/tv/b.mkv");
        store.upsert_file(&a).unwrap();
        store.upsert_file(&b).unwrap();

        let mut filters = FileFilters::default();
        filters.path_prefix = Some("/movies".into());
        let list = store.list_files(&filters).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, PathBuf::from("/movies/a.mkv"));
    }

    #[test]
    fn list_files_language_filter() {
        use voom_domain::media::{Track, TrackType};

        let store = test_store();
        let mut eng = MediaFile::new(PathBuf::from("/media/eng.mkv"));
        eng.content_hash = Some("h1".into());
        let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
        t.language = "eng".into();
        eng.tracks = vec![t];
        store.upsert_file(&eng).unwrap();

        let mut deu = MediaFile::new(PathBuf::from("/media/deu.mkv"));
        deu.content_hash = Some("h2".into());
        let mut t2 = Track::new(1, TrackType::AudioMain, "aac".into());
        t2.language = "deu".into();
        deu.tracks = vec![t2];
        store.upsert_file(&deu).unwrap();

        let mut filters = FileFilters::default();
        filters.has_language = Some("eng".into());
        let results = store.list_files(&filters).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, PathBuf::from("/media/eng.mkv"));
    }

    #[test]
    fn list_files_include_missing_flag() {
        let store = test_store();
        let active = active_file("/media/active.mkv");
        let soon_missing = active_file("/media/missing.mkv");
        store.upsert_file(&active).unwrap();
        store.upsert_file(&soon_missing).unwrap();
        store.mark_missing(&soon_missing.id).unwrap();

        let default_filters = FileFilters::default();
        let visible = store.list_files(&default_filters).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].path, PathBuf::from("/media/active.mkv"));

        let mut with_missing = FileFilters::default();
        with_missing.include_missing = true;
        let all = store.list_files(&with_missing).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn mark_missing_is_idempotent() {
        let store = test_store();
        let file = active_file("/media/file.mkv");
        store.upsert_file(&file).unwrap();
        store.mark_missing(&file.id).unwrap();
        // Second call should not fail or un-mark the file.
        store.mark_missing(&file.id).unwrap();

        let stored = store.file(&file.id).unwrap().unwrap();
        assert_eq!(stored.status, FileStatus::Missing);
    }

    #[test]
    fn reactivate_file_restores_active_status() {
        let store = test_store();
        let file = active_file("/media/old-path.mkv");
        store.upsert_file(&file).unwrap();
        store.mark_missing(&file.id).unwrap();

        let new_path = Path::new("/media/new-path.mkv");
        store.reactivate_file(&file.id, new_path).unwrap();

        let stored = store.file(&file.id).unwrap().unwrap();
        assert_eq!(stored.status, FileStatus::Active);
        assert_eq!(stored.path, new_path);
    }

    #[test]
    fn purge_missing_respects_cutoff() {
        use chrono::{Duration, Utc};

        let store = test_store();
        let file = active_file("/media/vanished.mkv");
        store.upsert_file(&file).unwrap();
        store.mark_missing(&file.id).unwrap();

        // Cutoff in the past — file missing_since is "now", so it should NOT be purged.
        let past = Utc::now() - Duration::hours(1);
        let purged = store.purge_missing(past).unwrap();
        assert_eq!(purged, 0, "cutoff in past must not purge current records");
        assert!(store.file(&file.id).unwrap().is_some());

        // Cutoff in the future — record's missing_since < future, so it IS purged.
        let future = Utc::now() + Duration::hours(1);
        let purged = store.purge_missing(future).unwrap();
        assert_eq!(purged, 1);
        assert!(store.file(&file.id).unwrap().is_none());
    }

    #[test]
    fn update_expected_hash_unknown_id_is_noop() {
        let store = test_store();
        // No file with this id exists — should quietly update 0 rows, no error.
        store
            .update_expected_hash(&Uuid::new_v4(), "arbitrary-hash")
            .unwrap();
    }

    #[test]
    fn update_expected_hash_writes_value() {
        let store = test_store();
        let file = active_file("/media/file.mkv");
        store.upsert_file(&file).unwrap();
        store.update_expected_hash(&file.id, "new-hash").unwrap();

        let stored = store.file(&file.id).unwrap().unwrap();
        assert_eq!(stored.expected_hash.as_deref(), Some("new-hash"));
    }

    #[test]
    fn record_post_execution_atomically_writes_all_three() {
        use voom_domain::storage::FileTransitionStorage;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mp4");
        store.upsert_file(&file).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap()
            .id;

        let transition = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".to_string(),
            2048,
            TransitionSource::Voom,
        )
        .with_from(Some("abc123".to_string()), Some(1024))
        .with_from_path(PathBuf::from("/media/movie.mp4"));

        store
            .record_post_execution(
                Some(Path::new("/media/movie.mkv")),
                Some("new_hash"),
                &transition,
            )
            .unwrap();

        // 1. Path is renamed
        assert!(
            store
                .file_by_path(Path::new("/media/movie.mp4"))
                .unwrap()
                .is_none()
        );
        let renamed = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(renamed.id, original_id);

        // 2. expected_hash is updated
        assert_eq!(renamed.expected_hash.as_deref(), Some("new_hash"));

        // 3. Transition is recorded with from_path preserved
        let transitions = store.transitions_for_file(&original_id).unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].to_hash, "new_hash");
        assert_eq!(
            transitions[0].from_path.as_deref(),
            Some(Path::new("/media/movie.mp4"))
        );
    }

    #[test]
    fn record_post_execution_no_path_change_skips_rename() {
        use voom_domain::storage::FileTransitionStorage;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mkv");
        store.upsert_file(&file).unwrap();
        let id = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap()
            .id;

        let transition = FileTransition::new(
            id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".to_string(),
            2048,
            TransitionSource::Voom,
        )
        .with_from(Some("abc123".to_string()), Some(1024));

        store
            .record_post_execution(None, Some("new_hash"), &transition)
            .unwrap();

        let stored = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(stored.id, id);
        assert_eq!(stored.expected_hash.as_deref(), Some("new_hash"));
        assert_eq!(store.transitions_for_file(&id).unwrap().len(), 1);
    }

    #[test]
    fn record_post_execution_no_hash_skips_expected_hash_update() {
        use voom_domain::storage::FileTransitionStorage;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mkv"); // active_file sets expected_hash="abc123"
        store.upsert_file(&file).unwrap();
        let id = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap()
            .id;
        // Set a known prior expected_hash directly so we can verify it's preserved.
        store.update_expected_hash(&id, "prior_hash").unwrap();

        let transition = FileTransition::new(
            id,
            PathBuf::from("/media/movie.mkv"),
            String::new(),
            2048,
            TransitionSource::Voom,
        );

        store
            .record_post_execution(None, None, &transition)
            .unwrap();

        let stored = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        // expected_hash MUST be untouched when None is passed
        assert_eq!(stored.expected_hash.as_deref(), Some("prior_hash"));
        // Transition still recorded
        assert_eq!(store.transitions_for_file(&id).unwrap().len(), 1);
    }

    #[test]
    fn record_post_execution_rolls_back_on_duplicate_transition_id() {
        use voom_domain::storage::FileTransitionStorage;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mp4");
        store.upsert_file(&file).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap()
            .id;
        store.update_expected_hash(&original_id, "abc123").unwrap();

        // Pre-insert a transition with a known id, then try to record_post_execution
        // with the same transition.id. The INSERT will violate the PK constraint,
        // and the entire bundle (rename + expected_hash) must roll back.
        let mut existing = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mp4"),
            "abc123".to_string(),
            1024,
            TransitionSource::Voom,
        );
        existing.id = uuid::Uuid::new_v4();
        store.record_transition(&existing).unwrap();

        let mut bundled = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".to_string(),
            2048,
            TransitionSource::Voom,
        );
        bundled.id = existing.id; // force a PK collision

        let result = store.record_post_execution(
            Some(Path::new("/media/movie.mkv")),
            Some("new_hash"),
            &bundled,
        );
        assert!(result.is_err(), "duplicate transition id must error");

        // Rollback verification: path NOT renamed, expected_hash NOT updated,
        // only the original transition exists.
        let still_at_old = store.file_by_path(Path::new("/media/movie.mp4")).unwrap();
        assert!(still_at_old.is_some(), "rename must have been rolled back");
        let stored = still_at_old.unwrap();
        assert_eq!(
            stored.expected_hash.as_deref(),
            Some("abc123"),
            "expected_hash must have been rolled back"
        );
        assert_eq!(
            store.transitions_for_file(&original_id).unwrap().len(),
            1,
            "only the pre-existing transition should remain"
        );
    }

    #[test]
    fn record_post_execution_clears_orphan_bad_files_row_at_destination() {
        use voom_domain::bad_file::{BadFile, BadFileSource};
        use voom_domain::storage::BadFileStorage;
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mp4");
        store.upsert_file(&file).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap()
            .id;

        // Pre-seed an orphan bad_files row at the post-execution path,
        // simulating a prior FileIntrospectionFailed dispatch. The bundled
        // rename must clear it.
        let orphan = BadFile::new(
            PathBuf::from("/media/movie.mkv"),
            2048,
            Some("post_exec_hash".into()),
            "ffprobe failed: process exited with code 1".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&orphan).unwrap();
        assert!(
            store
                .bad_file_by_path(Path::new("/media/movie.mkv"))
                .unwrap()
                .is_some(),
            "precondition: orphan bad_files row must exist before record_post_execution"
        );

        let transition = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".to_string(),
            2048,
            TransitionSource::Voom,
        )
        .with_from(Some("abc123".to_string()), Some(1024))
        .with_from_path(PathBuf::from("/media/movie.mp4"));

        store
            .record_post_execution(
                Some(Path::new("/media/movie.mkv")),
                Some("new_hash"),
                &transition,
            )
            .unwrap();

        // Files row was renamed (existing assertion shape from the
        // ..._atomically_writes_all_three test).
        let renamed = store
            .file_by_path(Path::new("/media/movie.mkv"))
            .unwrap()
            .unwrap();
        assert_eq!(renamed.id, original_id);

        // The orphan bad_files row at the post-execution path must be gone.
        assert!(
            store
                .bad_file_by_path(Path::new("/media/movie.mkv"))
                .unwrap()
                .is_none(),
            "record_post_execution must clear orphan bad_files row at the post-execution path"
        );
    }

    #[test]
    fn record_post_execution_rollback_preserves_bad_files_row() {
        use voom_domain::bad_file::{BadFile, BadFileSource};
        use voom_domain::storage::{BadFileStorage, FileTransitionStorage};
        use voom_domain::transition::{FileTransition, TransitionSource};

        let store = test_store();
        let file = active_file("/media/movie.mp4");
        store.upsert_file(&file).unwrap();
        let original_id = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .unwrap()
            .id;

        let orphan = BadFile::new(
            PathBuf::from("/media/movie.mkv"),
            2048,
            Some("post_exec_hash".into()),
            "ffprobe failed".into(),
            BadFileSource::Introspection,
        );
        store.upsert_bad_file(&orphan).unwrap();

        // Force the bundle's transition INSERT to fail by colliding on the
        // primary key with a pre-existing row. All four bundle effects
        // (rename, transition INSERT, expected_hash UPDATE, bad_files DELETE)
        // must roll back together.
        let mut existing = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mp4"),
            "abc123".to_string(),
            1024,
            TransitionSource::Voom,
        );
        existing.id = uuid::Uuid::new_v4();
        store.record_transition(&existing).unwrap();

        let mut bundled = FileTransition::new(
            original_id,
            PathBuf::from("/media/movie.mkv"),
            "new_hash".to_string(),
            2048,
            TransitionSource::Voom,
        );
        bundled.id = existing.id; // force a PK collision

        let result = store.record_post_execution(
            Some(Path::new("/media/movie.mkv")),
            Some("new_hash"),
            &bundled,
        );
        assert!(result.is_err(), "duplicate transition id must error");

        let still_at_old = store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .expect("rename must have been rolled back");
        // `upsert_file` doesn't persist `expected_hash`, so the pre-bundle
        // value is None; without rollback we'd observe Some("new_hash").
        assert_eq!(
            still_at_old.expected_hash, None,
            "expected_hash UPDATE must have been rolled back"
        );
        assert!(
            store
                .bad_file_by_path(Path::new("/media/movie.mkv"))
                .unwrap()
                .is_some(),
            "bad_files DELETE must have been rolled back"
        );
    }

    #[test]
    fn reconcile_discovered_files_basic_new_file() {
        let store = test_store();
        let discovered = vec![voom_domain::transition::DiscoveredFile::new(
            PathBuf::from("/media/new.mkv"),
            2048,
            "brand-new-hash".to_string(),
        )];
        let scanned = vec![PathBuf::from("/media")];
        let result = store
            .reconcile_discovered_files(&discovered, &scanned)
            .unwrap();

        assert_eq!(result.new_files, 1);
        assert_eq!(result.unchanged, 0);
        assert_eq!(result.moved, 0);
        assert_eq!(result.missing, 0);
        assert_eq!(result.external_changes, 0);
        assert_eq!(result.needs_introspection.len(), 1);
    }

    #[test]
    fn begin_scan_session_creates_in_progress_row() {
        use std::path::PathBuf;

        let store = test_store();
        let roots = vec![PathBuf::from("/movies")];
        let session = store.begin_scan_session(&roots).unwrap();

        let conn = store.conn().unwrap();
        let (status, roots_json): (String, String) = conn
            .query_row(
                "SELECT status, roots_json FROM scan_sessions WHERE id = ?1",
                params![session.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "in_progress");
        assert_eq!(roots_json, "[\"/movies\"]");
    }

    #[test]
    fn begin_scan_session_auto_cancels_prior_in_progress() {
        use std::path::PathBuf;

        let store = test_store();
        let first = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let second = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        assert_ne!(first.to_string(), second.to_string());

        let conn = store.conn().unwrap();
        let first_status: String = conn
            .query_row(
                "SELECT status FROM scan_sessions WHERE id = ?1",
                params![first.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(first_status, "cancelled");
        let first_finished_at: Option<String> = conn
            .query_row(
                "SELECT finished_at FROM scan_sessions WHERE id = ?1",
                params![first.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            first_finished_at.is_some(),
            "cancelled session must have finished_at set"
        );
        let second_status: String = conn
            .query_row(
                "SELECT status FROM scan_sessions WHERE id = ?1",
                params![second.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(second_status, "in_progress");
    }

    #[test]
    fn cancel_scan_session_marks_cancelled_and_leaves_files_unchanged() {
        use std::path::PathBuf;

        let store = test_store();
        let roots = vec![PathBuf::from("/movies")];

        // Seed an active file
        let f = active_file("/movies/a.mkv");
        store.upsert_file(&f).unwrap();

        let session = store.begin_scan_session(&roots).unwrap();
        store.cancel_scan_session(session).unwrap();

        let conn = store.conn().unwrap();
        let (status, finished_at): (String, Option<String>) = conn
            .query_row(
                "SELECT status, finished_at FROM scan_sessions WHERE id = ?1",
                params![session.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert!(
            finished_at.is_some(),
            "cancelled session must have finished_at set"
        );

        // file row is still active
        let file_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(file_status, "active");
    }

    #[test]
    fn finish_session_with_no_ingest_marks_all_files_in_roots_missing() {
        use std::path::PathBuf;

        let store = test_store();
        let roots = vec![PathBuf::from("/movies")];

        let f = active_file("/movies/a.mkv");
        store.upsert_file(&f).unwrap();
        let g = active_file("/other/b.mkv");
        store.upsert_file(&g).unwrap();

        let session = store.begin_scan_session(&roots).unwrap();
        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(
            finish.missing, 1,
            "only files under /movies should be marked missing"
        );

        let conn = store.conn().unwrap();
        let a_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_status, "missing");

        let b_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/other/b.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_status, "active");

        let session_status: String = conn
            .query_row(
                "SELECT status FROM scan_sessions WHERE id = ?1",
                params![session.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session_status, "completed");

        let finished_at: Option<String> = conn
            .query_row(
                "SELECT finished_at FROM scan_sessions WHERE id = ?1",
                params![session.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            finished_at.is_some(),
            "completed session must have finished_at set"
        );
    }

    #[test]
    fn finish_session_errors_if_not_in_progress() {
        use std::path::PathBuf;

        let store = test_store();
        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        store.finish_scan_session(session).unwrap();

        let err = store.finish_scan_session(session).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("not in_progress") || msg.contains("not in progress"),
            "expected 'not in_progress' in error, got: {msg}"
        );
    }

    #[test]
    fn ingest_new_file_inserts_and_returns_new() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), 100, "h-a".to_string());

        let decision = store.ingest_discovered_file(session, &df).unwrap();
        match decision {
            IngestDecision::New {
                needs_introspection,
                ..
            } => {
                assert!(needs_introspection);
            }
            other => panic!("expected New, got {other:?}"),
        }

        // File row created with status=active, last_seen_session_id stamped
        let conn = store.conn().unwrap();
        let (status, last_seen): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_seen_session_id FROM files WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert_eq!(last_seen.as_deref(), Some(session.to_string().as_str()));

        // Discovery transition recorded
        let tx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions \
                 WHERE source = 'discovery' AND path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tx_count, 1);
    }

    #[test]
    fn ingest_unchanged_file_stamps_session_and_backfills_expected_hash() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        // Seed an existing file with NULL expected_hash
        let mut f = active_file("/movies/a.mkv");
        f.expected_hash = None;
        // Make sure content_hash matches what we'll ingest
        f.content_hash = Some("h-a".to_string());
        store.upsert_file(&f).unwrap();

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), f.size, "h-a".to_string());

        let decision = store.ingest_discovered_file(session, &df).unwrap();
        assert!(matches!(decision, IngestDecision::Unchanged { .. }));

        let conn = store.conn().unwrap();
        let (expected_hash, last_seen): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT expected_hash, last_seen_session_id FROM files WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            expected_hash.as_deref(),
            Some("h-a"),
            "expected_hash should be backfilled"
        );
        assert_eq!(
            last_seen.as_deref(),
            Some(session.to_string().as_str()),
            "last_seen_session_id should be stamped"
        );

        // No new transition row for unchanged
        let tx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tx_count, 0, "unchanged should not create transitions");
    }

    #[test]
    fn ingest_externally_changed_creates_supersession_and_external_transition() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        let mut f = active_file("/movies/a.mkv");
        f.content_hash = Some("h-old".to_string());
        f.expected_hash = Some("h-old".to_string());
        store.upsert_file(&f).unwrap();
        // upsert_file does not write expected_hash; stamp it directly.
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET expected_hash = 'h-old' WHERE path = '/movies/a.mkv'",
                [],
            )
            .unwrap();
        }
        let old_id = f.id;

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), 150, "h-new".to_string());

        let decision = store.ingest_discovered_file(session, &df).unwrap();
        let (new_id, superseded) = match decision {
            IngestDecision::ExternallyChanged {
                file_id,
                superseded,
            } => (file_id, superseded),
            other => panic!("expected ExternallyChanged, got {other:?}"),
        };
        assert_eq!(superseded, old_id);
        assert_ne!(new_id, old_id);

        let conn = store.conn().unwrap();

        // External transition recorded
        let ext_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions \
                 WHERE source = 'external' AND path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ext_count, 1);

        // Discovery transition for the new row recorded
        let disc_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions \
                 WHERE source = 'discovery' AND path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(disc_count, 1);

        // last_seen_session_id stamped on the new row only
        let new_last_seen: Option<String> = conn
            .query_row(
                "SELECT last_seen_session_id FROM files WHERE id = ?1",
                params![new_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(new_last_seen.as_deref(), Some(session.to_string().as_str()));

        // Old row is superseded
        let (old_status, superseded_by): (String, Option<String>) = conn
            .query_row(
                "SELECT status, superseded_by FROM files WHERE id = ?1",
                params![old_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(old_status, "missing");
        assert_eq!(superseded_by.as_deref(), Some(new_id.to_string().as_str()));
    }

    #[test]
    fn ingest_moved_file_reuses_id_and_records_move_transition() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        // Seed a file marked missing with a known expected_hash
        let mut f = active_file("/movies/old.mkv");
        f.status = FileStatus::Missing;
        f.content_hash = Some("h-content".to_string());
        f.expected_hash = Some("h-content".to_string());
        store.upsert_file(&f).unwrap();
        let original_id = f.id;

        // upsert_file doesn't write expected_hash or status='missing', so set them
        // directly via SQL the way the ExternallyChanged test does.
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET expected_hash = ?1, status = 'missing' WHERE id = ?2",
                params!["h-content", original_id.to_string()],
            )
            .unwrap();
        }

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df = DiscoveredFile::new(
            PathBuf::from("/movies/new.mkv"),
            100,
            "h-content".to_string(),
        );

        let decision = store.ingest_discovered_file(session, &df).unwrap();
        let (file_id, from_path) = match decision {
            IngestDecision::Moved { file_id, from_path } => (file_id, from_path),
            other => panic!("expected Moved, got {other:?}"),
        };
        assert_eq!(file_id, original_id);
        assert_eq!(from_path, PathBuf::from("/movies/old.mkv"));

        let conn = store.conn().unwrap();
        let (path, status): (String, String) = conn
            .query_row(
                "SELECT path, status FROM files WHERE id = ?1",
                params![original_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, "/movies/new.mkv");
        assert_eq!(status, "active");

        // Discovery transition with from_path recorded
        let from_path_db: String = conn
            .query_row(
                "SELECT from_path FROM file_transitions WHERE file_id = ?1 AND path = ?2",
                params![original_id.to_string(), "/movies/new.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(from_path_db, "/movies/old.mkv");
    }

    #[test]
    fn ingest_does_not_claim_same_missing_row_twice() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        let f = active_file("/movies/old.mkv");
        let original_id = f.id;
        store.upsert_file(&f).unwrap();

        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET expected_hash = ?1, content_hash = ?2, status = 'missing' WHERE id = ?3",
                params!["h-content", "h-content", original_id.to_string()],
            )
            .unwrap();
        }

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();

        let df1 = DiscoveredFile::new(
            PathBuf::from("/movies/new_a.mkv"),
            100,
            "h-content".to_string(),
        );
        let df2 = DiscoveredFile::new(
            PathBuf::from("/movies/new_b.mkv"),
            100,
            "h-content".to_string(),
        );

        let d1 = store.ingest_discovered_file(session, &df1).unwrap();
        let d2 = store.ingest_discovered_file(session, &df2).unwrap();

        assert!(matches!(d1, IngestDecision::Moved { .. }));
        // The first one consumed the missing row by flipping it to active; the second
        // sees no missing match and must be a fresh New row.
        assert!(matches!(d2, IngestDecision::New { .. }), "got {d2:?}");
    }

    #[test]
    fn ingest_duplicate_path_in_same_session_returns_duplicate() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df = DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), 100, "h-a".to_string());

        let d1 = store.ingest_discovered_file(session, &df).unwrap();
        let d2 = store.ingest_discovered_file(session, &df).unwrap();

        let id1 = match d1 {
            IngestDecision::New { file_id, .. } => file_id,
            other => panic!("expected New first, got {other:?}"),
        };
        let id2 = match d2 {
            IngestDecision::Duplicate { file_id } => file_id,
            other => panic!("expected Duplicate second, got {other:?}"),
        };
        assert_eq!(id1, id2);

        // Only one transition recorded (the first New); no duplicate
        let conn = store.conn().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_transitions WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn finish_preserves_ingested_files_and_marks_only_unseen() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();
        // Two pre-existing active files under /movies.
        // active_file always sets content_hash = "abc123".
        let a = active_file("/movies/a.mkv");
        let b = active_file("/movies/b.mkv");
        store.upsert_file(&a).unwrap();
        store.upsert_file(&b).unwrap();

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        // Ingest `a` with matching size and hash so it resolves as Unchanged.
        let df_a =
            DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), a.size, "abc123".to_string());
        store.ingest_discovered_file(session, &df_a).unwrap();

        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(
            finish.missing, 1,
            "only b.mkv was unseen and must be marked missing"
        );

        let conn = store.conn().unwrap();
        let a_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/movies/a.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(a_status, "active");
        let b_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/movies/b.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_status, "missing");
    }

    #[test]
    fn cancel_session_leaves_unseen_files_active() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();
        let a = active_file("/movies/a.mkv");
        let b = active_file("/movies/b.mkv");
        store.upsert_file(&a).unwrap();
        store.upsert_file(&b).unwrap();

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        let df_a =
            DiscoveredFile::new(PathBuf::from("/movies/a.mkv"), a.size, "abc123".to_string());
        store.ingest_discovered_file(session, &df_a).unwrap();
        store.cancel_scan_session(session).unwrap();

        let conn = store.conn().unwrap();
        let b_status: String = conn
            .query_row(
                "SELECT status FROM files WHERE path = ?1",
                params!["/movies/b.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(b_status, "active", "cancel must not mark anything missing");
    }

    #[test]
    fn overlapping_scan_roots_double_ingest_yields_duplicate() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        let session = store
            .begin_scan_session(&[PathBuf::from("/movies"), PathBuf::from("/movies/4k")])
            .unwrap();

        let df = DiscoveredFile::new(PathBuf::from("/movies/4k/a.mkv"), 100, "h-a".to_string());

        // First call (e.g. from outer root walk)
        let d1 = store.ingest_discovered_file(session, &df).unwrap();
        assert!(matches!(d1, IngestDecision::New { .. }));

        // Second call (e.g. from inner root walk, same path)
        let d2 = store.ingest_discovered_file(session, &df).unwrap();
        assert!(matches!(d2, IngestDecision::Duplicate { .. }));

        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(
            finish.missing, 0,
            "the one seen file must not be marked missing"
        );
    }

    #[test]
    fn reconcile_wrapper_matches_session_api_outcomes() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();

        // Seed:
        //   - active a (will be Unchanged)
        //   - missing b with expected_hash (will be Moved to /m/new.mkv)
        //   - active c (will become missing)
        //   - active d with expected_hash='h-d-old' (will be ExternallyChanged)
        let a = active_file("/m/a.mkv");
        let b = active_file("/m/old.mkv");
        let c = active_file("/m/c.mkv");
        let d = active_file("/m/d.mkv");
        store.upsert_file(&a).unwrap();
        store.upsert_file(&b).unwrap();
        store.upsert_file(&c).unwrap();
        store.upsert_file(&d).unwrap();

        let a_hash: String = {
            let conn = store.conn().unwrap();
            conn.query_row(
                "SELECT content_hash FROM files WHERE path = ?1",
                params!["/m/a.mkv"],
                |row| row.get(0),
            )
            .unwrap()
        };

        // Stamp b as missing with expected_hash='h-moved'
        // Stamp d's content_hash/expected_hash='h-d-old' so a discovery with h-d-new triggers external change
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET status = 'missing', \
                 content_hash = 'h-moved', expected_hash = 'h-moved' \
                 WHERE path = ?1",
                params!["/m/old.mkv"],
            )
            .unwrap();
            conn.execute(
                "UPDATE files SET content_hash = 'h-d-old', expected_hash = 'h-d-old' \
                 WHERE path = ?1",
                params!["/m/d.mkv"],
            )
            .unwrap();
        }

        let brand_new = DiscoveredFile::new(
            PathBuf::from("/m/brand-new.mkv"),
            100,
            "h-fresh".to_string(),
        );
        let discovered = vec![
            DiscoveredFile::new(PathBuf::from("/m/a.mkv"), a.size, a_hash),
            DiscoveredFile::new(PathBuf::from("/m/new.mkv"), 100, "h-moved".to_string()),
            brand_new.clone(),
            brand_new, // duplicate path in input — should be silently dropped
            DiscoveredFile::new(PathBuf::from("/m/d.mkv"), d.size, "h-d-new".to_string()),
        ];
        let result = store
            .reconcile_discovered_files(&discovered, &[PathBuf::from("/m")])
            .unwrap();

        assert_eq!(result.new_files, 1, "brand-new.mkv");
        assert_eq!(result.unchanged, 1, "a.mkv");
        assert_eq!(result.moved, 1, "old.mkv -> new.mkv");
        assert_eq!(result.external_changes, 1, "d.mkv");
        assert_eq!(result.missing, 1, "c.mkv");
        // needs_introspection: new + moved + external_changes
        assert_eq!(result.needs_introspection.len(), 3);
    }

    #[test]
    fn move_detected_at_finish_when_old_file_was_active_before_scan() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();

        // Seed an ACTIVE file with a known expected_hash (mimics a previously
        // introspected file at the old path).
        let f = active_file("/movies/old.mkv");
        store.upsert_file(&f).unwrap();
        let original_id = f.id;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET content_hash = 'h-content', expected_hash = 'h-content' \
                 WHERE id = ?1",
                params![original_id.to_string()],
            )
            .unwrap();
        }

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();

        // Ingest the file at its NEW path (the move). Old path is NOT re-ingested
        // (the file is no longer there).
        let df_new = DiscoveredFile::new(
            PathBuf::from("/movies/new.mkv"),
            100,
            "h-content".to_string(),
        );
        store.ingest_discovered_file(session, &df_new).unwrap();

        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(
            finish.missing, 0,
            "no file should be missing after promotion"
        );
        assert_eq!(finish.promoted_moves, 1, "one move should be promoted");

        // The original file id should now point at the new path with active status.
        let conn = store.conn().unwrap();
        let (path, status): (String, String) = conn
            .query_row(
                "SELECT path, status FROM files WHERE id = ?1",
                params![original_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(path, "/movies/new.mkv");
        assert_eq!(status, "active");

        // The move transition should exist with from_path set.
        let from_path: String = conn
            .query_row(
                "SELECT from_path FROM file_transitions \
                 WHERE file_id = ?1 AND path = ?2",
                params![original_id.to_string(), "/movies/new.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(from_path, "/movies/old.mkv");

        // There should be exactly one file row for the new path (no leftover New row).
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = ?1",
                params!["/movies/new.mkv"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "only the promoted candidate row, no separate New row"
        );
    }

    #[test]
    fn ingest_moved_does_not_steal_from_other_root() {
        use std::path::PathBuf;
        use voom_domain::transition::{DiscoveredFile, IngestDecision};

        let store = test_store();
        // Pre-existing TV file marked missing with a known hash — NOT under
        // the scan root we're about to use.
        let mut tv = active_file("/tv/old-show.mkv");
        tv.content_hash = Some("h-shared".to_string());
        store.upsert_file(&tv).unwrap();
        let tv_id = tv.id;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET status = 'missing', \
                 content_hash = 'h-shared', expected_hash = 'h-shared' \
                 WHERE id = ?1",
                params![tv_id.to_string()],
            )
            .unwrap();
        }

        // Scan /movies only.
        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        // Ingest a file in /movies with the same hash — NOT a move, just a coincidence.
        let df = DiscoveredFile::new(
            PathBuf::from("/movies/foo.mkv"),
            100,
            "h-shared".to_string(),
        );
        let decision = store.ingest_discovered_file(session, &df).unwrap();
        assert!(
            matches!(decision, IngestDecision::New { .. }),
            "cross-root hash collision must NOT be treated as a move; got {decision:?}",
        );

        // Confirm the TV row is still in its original state.
        let conn = store.conn().unwrap();
        let (tv_path, tv_status): (String, String) = conn
            .query_row(
                "SELECT path, status FROM files WHERE id = ?1",
                params![tv_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            tv_path, "/tv/old-show.mkv",
            "TV row must not be moved by /movies scan"
        );
        assert_eq!(tv_status, "missing");

        // Finish — TV file is not under scan root, so it should stay missing
        // (the missing pass only updates files under session roots).
        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(finish.missing, 0);
        assert_eq!(finish.promoted_moves, 0);

        let (tv_path_after, tv_status_after): (String, String) = conn
            .query_row(
                "SELECT path, status FROM files WHERE id = ?1",
                params![tv_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(tv_path_after, "/tv/old-show.mkv");
        assert_eq!(tv_status_after, "missing");
    }

    #[test]
    fn finish_promotion_does_not_steal_externally_changed_replacement() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();
        // Pre-existing /a.mkv with hash X (will be externally changed to hash Y)
        let mut a = active_file("/a.mkv");
        a.content_hash = Some("h-x".to_string());
        store.upsert_file(&a).unwrap();
        let a_id = a.id;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET content_hash = 'h-x', expected_hash = 'h-x' WHERE id = ?1",
                params![a_id.to_string()],
            )
            .unwrap();
        }

        // Pre-existing /b.mkv with hash Y and expected_hash Y (will NOT be re-ingested)
        let mut b = active_file("/b.mkv");
        b.content_hash = Some("h-y".to_string());
        store.upsert_file(&b).unwrap();
        let b_id = b.id;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET content_hash = 'h-y', expected_hash = 'h-y' WHERE id = ?1",
                params![b_id.to_string()],
            )
            .unwrap();
        }

        // Scan root '/' covers both files.
        let session = store.begin_scan_session(&[PathBuf::from("/")]).unwrap();

        // Ingest /a.mkv with NEW hash Y → ExternallyChanged
        let df_a = DiscoveredFile::new(PathBuf::from("/a.mkv"), 100, "h-y".to_string());
        let _decision_a = store.ingest_discovered_file(session, &df_a).unwrap();

        // /b.mkv is NOT ingested — it will become a missing candidate.

        let finish = store.finish_scan_session(session).unwrap();

        // /b.mkv must be marked missing, NOT promoted to a move of the
        // ExternallyChanged replacement row.
        assert_eq!(
            finish.promoted_moves, 0,
            "ExternallyChanged replacement must not be hijacked by move promotion",
        );
        assert_eq!(finish.missing, 1, "/b.mkv must be marked missing");

        let conn = store.conn().unwrap();
        // /b.mkv row is still at /b.mkv, marked missing.
        let (b_path, b_status): (String, String) = conn
            .query_row(
                "SELECT path, status FROM files WHERE id = ?1",
                params![b_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(b_path, "/b.mkv");
        assert_eq!(b_status, "missing");

        // /a.mkv supersession is intact: one row at /a.mkv (the replacement), one
        // superseded row (old a) with status=missing and path=NULL.
        let active_a_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE path = '/a.mkv' AND status = 'active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active_a_count, 1,
            "active /a.mkv replacement must still exist"
        );

        let superseded_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE superseded_by IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(superseded_count, 1, "supersession chain intact");
    }

    #[test]
    fn finish_promoted_move_updates_content_hash() {
        use std::path::PathBuf;
        use voom_domain::transition::DiscoveredFile;

        let store = test_store();
        // Pre-existing active file with content_hash != expected_hash
        // (mimics a prior voom operation that bumped expected_hash).
        let mut f = active_file("/movies/old.mkv");
        f.content_hash = Some("h-stale".to_string());
        store.upsert_file(&f).unwrap();
        let original_id = f.id;
        {
            let conn = store.conn().unwrap();
            conn.execute(
                "UPDATE files SET content_hash = 'h-stale', expected_hash = 'h-current' \
                 WHERE id = ?1",
                params![original_id.to_string()],
            )
            .unwrap();
        }

        let session = store
            .begin_scan_session(&[PathBuf::from("/movies")])
            .unwrap();
        // Ingest a NEW path with content_hash matching expected_hash.
        let df_new = DiscoveredFile::new(
            PathBuf::from("/movies/new.mkv"),
            100,
            "h-current".to_string(),
        );
        store.ingest_discovered_file(session, &df_new).unwrap();
        let finish = store.finish_scan_session(session).unwrap();
        assert_eq!(finish.promoted_moves, 1);

        let conn = store.conn().unwrap();
        let (path, content_hash, expected_hash): (String, String, Option<String>) = conn
            .query_row(
                "SELECT path, content_hash, expected_hash FROM files WHERE id = ?1",
                params![original_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(path, "/movies/new.mkv");
        assert_eq!(
            content_hash, "h-current",
            "promoted move must update content_hash to the discovered hash"
        );
        assert_eq!(expected_hash.as_deref(), Some("h-current"));
    }
}
