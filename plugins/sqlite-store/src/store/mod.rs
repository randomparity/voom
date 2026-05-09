mod core;
mod files;
mod history;
mod jobs;
mod plans;
mod row_mappers;
mod sql;

use std::collections::HashMap;

use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::media::Track;

pub use core::{SqliteStore, SqliteStoreConfig};
pub(crate) use row_mappers::{
    checked_i64_to_u64, checked_optional_i64_to_u32, checked_optional_i64_to_u64,
    parse_optional_datetime, parse_required_datetime, row_to_bad_file, row_to_file, row_to_job,
    row_to_track, row_to_verification, row_uuid, FileRow,
};
pub(crate) use sql::{
    escape_like, format_datetime, other_storage_err, parse_datetime, parse_uuid, storage_err,
    OptionalExt, SqlQuery,
};

// Private helper methods
impl SqliteStore {
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
mod integration_tests;
