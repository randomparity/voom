use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::media::MediaFile;
use voom_domain::storage::{FileFilters, FileStorage};
use voom_domain::transition::{DiscoveredFile, FileTransition, ReconcileResult, TransitionSource};

use super::{
    escape_like, format_datetime, other_storage_err, row_to_file, storage_err, FileRow,
    OptionalExt, SqlQuery, SqliteStore,
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

    fn predecessor_of(&self, successor_id: &Uuid) -> Result<Option<MediaFile>> {
        let conn = self.conn()?;
        let file_row: Option<FileRow> = conn
            .query_row(
                "SELECT id, path, size, content_hash, expected_hash, status, \
                 container, duration, bitrate, tags, plugin_metadata, introspected_at \
                 FROM files WHERE superseded_by = ?1",
                params![successor_id.to_string()],
                row_to_file,
            )
            .optional()
            .map_err(storage_err("failed to query predecessor"))?;

        match file_row {
            Some(fr) => {
                let id = super::parse_uuid(&fr.id)?;
                let tracks = self.load_tracks(&conn, &id)?;
                Ok(Some(fr.to_media_file(tracks)?))
            }
            None => Ok(None),
        }
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

/// Build a hash-to-(id, size) index of missing files scoped to scanned dirs.
fn build_missing_hash_index(
    tx: &rusqlite::Transaction<'_>,
    scanned_dirs: &[PathBuf],
) -> Result<HashMap<String, (String, i64)>> {
    let mut stmt = tx
        .prepare(
            "SELECT id, path, expected_hash, size FROM files \
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
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(storage_err("failed to query missing files"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err("failed to collect missing files"))?;

    let mut map = HashMap::new();
    for (id, path, hash, size) in rows {
        let path_obj = Path::new(&path);
        if scanned_dirs.iter().any(|dir| path_obj.starts_with(dir)) {
            map.entry(hash).or_insert((id, size));
        }
    }
    Ok(map)
}

/// Pass 2: Match each discovered file against DB (unchanged, external change, move, or new).
fn match_discovered_files(
    tx: &rusqlite::Transaction<'_>,
    discovered: &[DiscoveredFile],
    missing_by_hash: &HashMap<String, (String, i64)>,
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
    let filename = df
        .path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
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

        tx.execute(
            "UPDATE files SET path = NULL, status = 'missing', \
             missing_since = ?1 WHERE id = ?2",
            params![now, existing_id],
        )
        .map_err(storage_err("failed to clear old file for external change"))?;

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
    missing_by_hash: &HashMap<String, (String, i64)>,
    consumed_missing: &mut HashSet<String>,
    now: &str,
    result: &mut ReconcileResult,
) -> Result<()> {
    let path_str = df.path.to_string_lossy().to_string();
    let filename = df
        .path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let move_match = missing_by_hash
        .get(&df.content_hash)
        .filter(|(id, _)| !consumed_missing.contains(id))
        .map(|(id, size)| (id.clone(), *size));

    if let Some((missing_id, _missing_size)) = move_match {
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
         (id, file_id, path, from_hash, to_hash, from_size, to_size, \
          source, source_detail, plan_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            t.id.to_string(),
            t.file_id.to_string(),
            t.path.to_string_lossy().to_string(),
            t.from_hash.as_deref(),
            t.to_hash,
            t.from_size.map(|v| v as i64),
            t.to_size as i64,
            t.source.as_str(),
            t.source_detail.as_deref(),
            t.plan_id.map(|id| id.to_string()),
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
}
