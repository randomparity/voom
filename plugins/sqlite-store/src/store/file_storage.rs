use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::media::MediaFile;
use voom_domain::storage::{FileFilters, FileStorage};
use voom_domain::transition::{DiscoveredFile, ReconcileResult};

use super::{
    escape_like, format_datetime, other_storage_err, row_to_file, storage_err, FileRow, SqlQuery,
    SqliteStore,
};

impl FileStorage for SqliteStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let tags_json = serde_json::to_string(&file.tags)
            .map_err(other_storage_err("failed to serialize tags"))?;
        let meta_json = serde_json::to_string(&file.plugin_metadata)
            .map_err(other_storage_err("failed to serialize metadata"))?;
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
            "INSERT INTO files (id, path, filename, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(path) DO UPDATE SET
                filename = excluded.filename,
                size = excluded.size,
                content_hash = excluded.content_hash,
                expected_hash = excluded.expected_hash,
                status = excluded.status,
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
                file.content_hash.as_deref().unwrap_or(""),
                file.expected_hash.as_deref(),
                file.status.as_str(),
                file.container.as_str(),
                file.duration,
                file.bitrate.map(i64::from),
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
                    "INSERT INTO tracks (id, file_id, stream_index, track_type, codec, language, title, is_default, is_forced, channels, channel_layout, sample_rate, bit_depth, width, height, frame_rate, is_vfr, is_hdr, hdr_format, pixel_format)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
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
                    track.is_default as i64,
                    track.is_forced as i64,
                    track.channels.map(i64::from),
                    track.channel_layout,
                    track.sample_rate.map(i64::from),
                    track.bit_depth.map(i64::from),
                    track.width.map(i64::from),
                    track.height.map(i64::from),
                    track.frame_rate,
                    track.is_vfr as i64,
                    track.is_hdr as i64,
                    track.hdr_format,
                    track.pixel_format,
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
                "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE id = ?1",
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
                "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE path = ?1",
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

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let conn = self.conn()?;
        let has_track_filter = filters.has_codec.is_some() || filters.has_language.is_some();

        // When filtering by codec/language, use a subquery to apply filters before
        // LIMIT/OFFSET, ensuring consistent pagination with count_files.
        let base = if has_track_filter {
            "SELECT DISTINCT files.id, files.path, files.size, files.content_hash, files.expected_hash, files.status, files.container, files.duration, files.bitrate, files.tags, files.plugin_metadata, files.introspected_at FROM files INNER JOIN tracks ON tracks.file_id = files.id WHERE 1=1"
        } else {
            "SELECT id, path, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at FROM files WHERE 1=1"
        };
        let mut q = SqlQuery::new(base);

        let col_prefix = if has_track_filter { "files." } else { "" };

        if !filters.include_missing {
            let clause = format!(" AND {col_prefix}status = {{}}");
            q.condition(&clause, "active".to_string());
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
            q.condition(" AND files.status = {}", "active".to_string());
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
            "UPDATE files SET status = 'missing', missing_since = ?1 WHERE id = ?2 AND status = 'active'",
            params![now, id.to_string()],
        )
        .map_err(storage_err("failed to mark file missing"))?;
        Ok(())
    }

    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let path_str = new_path.to_string_lossy().to_string();
        let filename = new_path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        conn.execute(
            "UPDATE files SET status = 'active', missing_since = NULL, path = ?1, filename = ?2, updated_at = ?3 WHERE id = ?4",
            params![path_str, filename, now, id.to_string()],
        )
        .map_err(storage_err("failed to reactivate file"))?;
        Ok(())
    }

    fn purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn()?;
        let cutoff = format_datetime(&older_than);
        conn.execute(
            "DELETE FROM file_transitions WHERE file_id IN (SELECT id FROM files WHERE status = 'missing' AND missing_since < ?1)",
            params![cutoff],
        )
        .map_err(storage_err("failed to purge transitions for missing files"))?;
        let deleted = conn
            .execute(
                "DELETE FROM files WHERE status = 'missing' AND missing_since < ?1",
                params![cutoff],
            )
            .map_err(storage_err("failed to purge missing files"))?;
        Ok(deleted as u64)
    }

    fn reconcile_discovered_files(
        &self,
        _discovered: &[DiscoveredFile],
        _scanned_dirs: &[PathBuf],
    ) -> Result<ReconcileResult> {
        // Stub: real implementation in Task 5
        Ok(ReconcileResult::default())
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
}

use super::OptionalExt;
