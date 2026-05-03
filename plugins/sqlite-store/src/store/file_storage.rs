use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::media::{MediaFile, StoredFingerprint};
use voom_domain::storage::{FileFilters, FileStorage};
use voom_domain::transition::{DiscoveredFile, FileTransition, ReconcileResult, TransitionSource};

use super::{
    escape_like, format_datetime, other_storage_err, parse_datetime, row_to_file, storage_err,
    FileRow, OptionalExt, SqlQuery, SqliteStore,
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
            "INSERT INTO files (id, path, filename, size, content_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(path) DO UPDATE SET
                filename = excluded.filename,
                size = excluded.size,
                content_hash = excluded.content_hash,
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
                    i64::from(track.is_default),
                    i64::from(track.is_forced),
                    track.channels.map(i64::from),
                    track.channel_layout,
                    track.sample_rate.map(i64::from),
                    track.bit_depth.map(i64::from),
                    track.width.map(i64::from),
                    track.height.map(i64::from),
                    track.frame_rate,
                    i64::from(track.is_vfr),
                    i64::from(track.is_hdr),
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
        let filename = filename_string(new_path);
        conn.execute(
            "UPDATE files SET status = 'active', missing_since = NULL, path = ?1, filename = ?2, updated_at = ?3 WHERE id = ?4",
            params![path_str, filename, now, id.to_string()],
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
        discovered: &[DiscoveredFile],
        scanned_dirs: &[PathBuf],
    ) -> Result<ReconcileResult> {
        let mut conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let tx = conn
            .transaction()
            .map_err(storage_err("failed to begin reconcile transaction"))?;

        let mut result = ReconcileResult::default();

        let discovered_paths: HashSet<String> = discovered
            .iter()
            .map(|d| d.path.to_string_lossy().to_string())
            .collect();

        result.missing = mark_missing_files(&tx, scanned_dirs, &discovered_paths, &now)?;
        let missing_by_hash = build_missing_hash_index(&tx, scanned_dirs)?;
        match_discovered_files(&tx, discovered, &missing_by_hash, &now, &mut result)?;

        tx.commit()
            .map_err(storage_err("failed to commit reconciliation"))?;
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

    fn record_post_execution(
        &self,
        id: &Uuid,
        new_path: Option<&Path>,
        new_expected_hash: Option<&str>,
        transition: &FileTransition,
    ) -> Result<()> {
        debug_assert_eq!(
            id, &transition.file_id,
            "record_post_execution: id must match transition.file_id"
        );

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
                params![path_str, filename, now, id.to_string()],
            )
            .map_err(storage_err("failed to rename file path in bundle"))?;
        }

        super::file_transition_storage::insert_full_transition_in_tx(&tx, transition)?;

        if let Some(hash) = new_expected_hash {
            tx.execute(
                "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
                params![hash, id.to_string()],
            )
            .map_err(storage_err("failed to update expected_hash in bundle"))?;
        }

        tx.commit()
            .map_err(storage_err("failed to commit post-execution bundle"))?;
        Ok(())
    }

    fn predecessor_of(&self, successor_id: &Uuid) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, expected_hash, status, \
                 container, duration, bitrate, tags, plugin_metadata, introspected_at \
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
}

/// Pass 1: Mark active files under scanned dirs as missing if not in discovered set.
fn mark_missing_files(
    tx: &rusqlite::Transaction<'_>,
    scanned_dirs: &[PathBuf],
    discovered_paths: &HashSet<String>,
    now: &str,
) -> Result<u32> {
    let mut stmt = tx
        .prepare(
            "SELECT id, path, expected_hash, size FROM files \
             WHERE status = 'active' AND path IS NOT NULL",
        )
        .map_err(storage_err("failed to prepare missing scan"))?;

    let active_files: Vec<(String, String, Option<String>, i64)> = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(storage_err("failed to query active files"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err("failed to collect active files"))?;

    let mut missing = 0u32;
    for (id, path, _expected_hash, _size) in &active_files {
        let path_obj = Path::new(path);
        let under_scanned = scanned_dirs.iter().any(|dir| path_obj.starts_with(dir));
        if under_scanned && !discovered_paths.contains(path.as_str()) {
            tx.execute(
                "UPDATE files SET status = 'missing', missing_since = ?1 \
                 WHERE id = ?2 AND status = 'active'",
                params![now, id],
            )
            .map_err(storage_err("failed to mark file missing"))?;
            missing += 1;
        }
    }
    Ok(missing)
}

/// A move-detection candidate: a missing file's identity plus its prior path,
/// so the resulting transition can record `from_path`.
struct MissingMatch {
    id: String,
    prior_path: String,
}

/// Build a content-hash → `MissingMatch` index of missing files scoped to
/// scanned dirs.
fn build_missing_hash_index(
    tx: &rusqlite::Transaction<'_>,
    scanned_dirs: &[PathBuf],
) -> Result<HashMap<String, MissingMatch>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, path, expected_hash FROM files \
             WHERE status = 'missing' AND expected_hash IS NOT NULL \
             AND path IS NOT NULL",
        )
        .map_err(storage_err("failed to prepare missing lookup"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(storage_err("failed to query missing files"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err("failed to collect missing files"))?;

    let mut map = HashMap::new();
    for (id, path, hash) in rows {
        let path_obj = Path::new(&path);
        if scanned_dirs.iter().any(|dir| path_obj.starts_with(dir)) {
            map.entry(hash).or_insert(MissingMatch {
                id,
                prior_path: path,
            });
        }
    }
    Ok(map)
}

/// Pass 2: Match each discovered file against DB (unchanged, external change, move, or new).
fn match_discovered_files(
    tx: &rusqlite::Transaction<'_>,
    discovered: &[DiscoveredFile],
    missing_by_hash: &HashMap<String, MissingMatch>,
    now: &str,
    result: &mut ReconcileResult,
) -> Result<()> {
    let mut consumed_missing: HashSet<String> = HashSet::new();

    for df in discovered {
        let path_str = df.path.to_string_lossy().to_string();

        let existing: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT id, expected_hash, size FROM files WHERE path = ?1",
                params![&path_str],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err("failed to check existing file"))?;

        if let Some((existing_id, expected_hash, existing_size)) = existing {
            reconcile_existing_path(
                tx,
                df,
                &existing_id,
                expected_hash,
                existing_size,
                now,
                result,
            )?;
        } else {
            reconcile_new_path(tx, df, missing_by_hash, &mut consumed_missing, now, result)?;
        }
    }
    Ok(())
}

/// Handle a discovered file whose path already exists in the DB.
fn reconcile_existing_path(
    tx: &rusqlite::Transaction<'_>,
    df: &DiscoveredFile,
    existing_id: &str,
    expected_hash: Option<String>,
    existing_size: i64,
    now: &str,
    result: &mut ReconcileResult,
) -> Result<()> {
    let path_str = df.path.to_string_lossy().to_string();
    let filename = filename_string(&df.path);
    let hash_matches = expected_hash
        .as_ref()
        .is_none_or(|eh| eh == &df.content_hash);

    if hash_matches {
        tx.execute(
            "UPDATE files SET size = ?1, content_hash = ?2, \
             status = 'active', missing_since = NULL, \
             updated_at = ?3 WHERE id = ?4",
            params![df.size as i64, &df.content_hash, now, existing_id],
        )
        .map_err(storage_err("failed to update unchanged file"))?;

        if expected_hash.is_none() {
            tx.execute(
                "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
                params![&df.content_hash, existing_id],
            )
            .map_err(storage_err("failed to backfill expected_hash"))?;
        }
        result.unchanged += 1;
    } else {
        let old_id = super::parse_uuid(existing_id)?;
        let ext_transition = FileTransition::new(
            old_id,
            df.path.clone(),
            df.content_hash.clone(),
            df.size,
            TransitionSource::External,
        )
        .with_from(expected_hash, Some(existing_size as u64));
        insert_transition_in_tx(tx, &ext_transition, now)?;

        let new_id = Uuid::new_v4();
        tx.execute(
            "UPDATE files SET path = NULL, status = 'missing', \
             missing_since = ?1, superseded_by = ?2 WHERE id = ?3",
            params![now, new_id.to_string(), existing_id],
        )
        .map_err(storage_err("failed to clear old file for external change"))?;
        tx.execute(
            "INSERT INTO files \
             (id, path, filename, size, content_hash, \
              expected_hash, status, container, duration, \
              tags, plugin_metadata, introspected_at, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', \
                     'other', 0.0, '{}', '{}', ?7, ?7, ?7)",
            params![
                new_id.to_string(),
                path_str,
                filename,
                df.size as i64,
                &df.content_hash,
                &df.content_hash,
                now,
            ],
        )
        .map_err(storage_err("failed to insert new file for external change"))?;

        let disc_transition = FileTransition::new(
            new_id,
            df.path.clone(),
            df.content_hash.clone(),
            df.size,
            TransitionSource::Discovery,
        );
        insert_transition_in_tx(tx, &disc_transition, now)?;

        result.external_changes += 1;
        result.needs_introspection.push(df.path.clone());
    }
    Ok(())
}

/// Handle a discovered file whose path is not yet in the DB (move or new).
fn reconcile_new_path(
    tx: &rusqlite::Transaction<'_>,
    df: &DiscoveredFile,
    missing_by_hash: &HashMap<String, MissingMatch>,
    consumed_missing: &mut HashSet<String>,
    now: &str,
    result: &mut ReconcileResult,
) -> Result<()> {
    let path_str = df.path.to_string_lossy().to_string();
    let filename = filename_string(&df.path);
    let move_match = missing_by_hash
        .get(&df.content_hash)
        .filter(|m| !consumed_missing.contains(&m.id));

    if let Some(m) = move_match {
        let missing_id = m.id.clone();
        let prior_path = m.prior_path.clone();
        consumed_missing.insert(missing_id.clone());

        tx.execute(
            "UPDATE files SET path = ?1, filename = ?2, \
             size = ?3, content_hash = ?4, \
             status = 'active', missing_since = NULL, \
             updated_at = ?5 WHERE id = ?6",
            params![
                path_str,
                filename,
                df.size as i64,
                &df.content_hash,
                now,
                &missing_id,
            ],
        )
        .map_err(storage_err("failed to reactivate moved file"))?;

        let file_uuid = super::parse_uuid(&missing_id)?;
        let move_transition = FileTransition::new(
            file_uuid,
            df.path.clone(),
            df.content_hash.clone(),
            df.size,
            TransitionSource::Discovery,
        )
        .with_from_path(PathBuf::from(prior_path))
        .with_detail("detected_move");
        insert_transition_in_tx(tx, &move_transition, now)?;

        result.moved += 1;
        result.needs_introspection.push(df.path.clone());
    } else {
        let new_id = Uuid::new_v4();
        tx.execute(
            "INSERT INTO files \
             (id, path, filename, size, content_hash, \
              expected_hash, status, container, duration, \
              tags, plugin_metadata, introspected_at, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', \
                     'other', 0.0, '{}', '{}', ?7, ?7, ?7)",
            params![
                new_id.to_string(),
                path_str,
                filename,
                df.size as i64,
                &df.content_hash,
                &df.content_hash,
                now,
            ],
        )
        .map_err(storage_err("failed to insert new file"))?;

        let disc_transition = FileTransition::new(
            new_id,
            df.path.clone(),
            df.content_hash.clone(),
            df.size,
            TransitionSource::Discovery,
        );
        insert_transition_in_tx(tx, &disc_transition, now)?;

        result.new_files += 1;
        result.needs_introspection.push(df.path.clone());
    }
    Ok(())
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
        assert!(store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .is_none());

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
                &original_id,
                Some(Path::new("/media/movie.mkv")),
                Some("new_hash"),
                &transition,
            )
            .unwrap();

        // 1. Path is renamed
        assert!(store
            .file_by_path(Path::new("/media/movie.mp4"))
            .unwrap()
            .is_none());
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
            .record_post_execution(&id, None, Some("new_hash"), &transition)
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
            .record_post_execution(&id, None, None, &transition)
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
            &original_id,
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
}
