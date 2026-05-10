//! CRUD operations for the `subtitles` table.

use rusqlite::params;

use voom_domain::errors::Result;

use super::{SqliteStore, storage_err};

/// A row from the `subtitles` table.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SubtitleRecord {
    pub id: i64,
    pub file_path: String,
    pub subtitle_path: String,
    pub language: String,
    pub forced: bool,
    pub title: Option<String>,
    pub created_at: String,
}

impl SqliteStore {
    /// Insert a subtitle record. Duplicates (same `file_path` + `subtitle_path`)
    /// are replaced with the latest data.
    pub fn upsert_subtitle(
        &self,
        file_path: &str,
        subtitle_path: &str,
        language: &str,
        forced: bool,
        title: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO subtitles (file_path, subtitle_path, language, forced, title)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![file_path, subtitle_path, language, i32::from(forced), title],
        )
        .map_err(storage_err("failed to upsert subtitle"))?;
        Ok(())
    }

    /// List all subtitle records for a given media file path.
    pub fn list_subtitles(&self, file_path: &str) -> Result<Vec<SubtitleRecord>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, subtitle_path, language, forced, title, created_at
                 FROM subtitles WHERE file_path = ?1 ORDER BY id",
            )
            .map_err(storage_err("failed to prepare subtitle query"))?;

        let records = stmt
            .query_map(params![file_path], |row| {
                Ok(SubtitleRecord {
                    id: row.get("id")?,
                    file_path: row.get("file_path")?,
                    subtitle_path: row.get("subtitle_path")?,
                    language: row.get("language")?,
                    forced: row.get::<_, i32>("forced")? != 0,
                    title: row.get("title")?,
                    created_at: row.get("created_at")?,
                })
            })
            .map_err(storage_err("failed to query subtitles"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect subtitles"))?;

        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SqliteStore {
        SqliteStore::in_memory().unwrap()
    }

    #[test]
    fn test_upsert_and_query_subtitle() {
        let store = test_store();
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.eng.srt",
                "eng",
                true,
                None,
            )
            .unwrap();

        let records = store.list_subtitles("/media/movie.mkv").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].file_path, "/media/movie.mkv");
        assert_eq!(records[0].subtitle_path, "/media/movie.eng.srt");
        assert_eq!(records[0].language, "eng");
        assert!(records[0].forced);
        assert!(records[0].title.is_none());
    }

    #[test]
    fn test_upsert_subtitle_with_title() {
        let store = test_store();
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.eng.srt",
                "eng",
                false,
                Some("Forced English"),
            )
            .unwrap();

        let records = store.list_subtitles("/media/movie.mkv").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].title.as_deref(), Some("Forced English"));
        assert!(!records[0].forced);
    }

    #[test]
    fn test_list_subtitles_empty() {
        let store = test_store();
        let records = store.list_subtitles("/nonexistent.mkv").unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn test_multiple_subtitles_per_file() {
        let store = test_store();
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.eng.srt",
                "eng",
                true,
                None,
            )
            .unwrap();
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.jpn.srt",
                "jpn",
                false,
                None,
            )
            .unwrap();

        let records = store.list_subtitles("/media/movie.mkv").unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn test_upsert_replaces_duplicate() {
        let store = test_store();
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.eng.srt",
                "eng",
                false,
                None,
            )
            .unwrap();
        // Re-upsert same (file_path, subtitle_path) with different data.
        store
            .upsert_subtitle(
                "/media/movie.mkv",
                "/media/movie.eng.srt",
                "eng",
                true,
                Some("SDH"),
            )
            .unwrap();

        let records = store.list_subtitles("/media/movie.mkv").unwrap();
        assert_eq!(records.len(), 1, "should replace, not duplicate");
        assert!(records[0].forced);
        assert_eq!(records[0].title.as_deref(), Some("SDH"));
    }

    #[test]
    fn test_subtitles_isolated_between_files() {
        let store = test_store();
        store
            .upsert_subtitle("/media/a.mkv", "/media/a.eng.srt", "eng", true, None)
            .unwrap();
        store
            .upsert_subtitle("/media/b.mkv", "/media/b.eng.srt", "eng", false, None)
            .unwrap();

        let a_records = store.list_subtitles("/media/a.mkv").unwrap();
        assert_eq!(a_records.len(), 1);
        let b_records = store.list_subtitles("/media/b.mkv").unwrap();
        assert_eq!(b_records.len(), 1);
    }
}
