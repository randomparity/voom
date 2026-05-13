//! Storage for `VoomOriginatedMutation` records, keyed by `(session, path)`.
//!
//! These rows let the scanner and `finish_scan_session` distinguish VOOM's own
//! filesystem writes from external changes.

use std::path::{Path, PathBuf};

use rusqlite::params;

use voom_domain::errors::{Result, StorageErrorKind, VoomError};
use voom_domain::scan_session_mutations::{MutationKind, VoomOriginatedMutation};
use voom_domain::transition::ScanSessionId;

use super::SqliteStore;
use crate::store::{other_storage_err, storage_err};

/// Storage trait surface for VOOM-originated mutations.
// Used by Task 4 (finish_scan_session) and later by executor/pipeline consumers
// via a deliberate `pub use` in lib.rs. Suppressed until that wiring lands.
#[allow(dead_code)]
pub(crate) trait ScanSessionMutationStorage {
    fn record_voom_mutation(&self, m: &VoomOriginatedMutation) -> Result<()>;
    fn is_voom_originated(&self, session: ScanSessionId, path: &Path) -> Result<bool>;
    fn voom_mutations_for_session(
        &self,
        session: ScanSessionId,
    ) -> Result<Vec<VoomOriginatedMutation>>;
}

/// Explicit, non-serde string mapping for `MutationKind` storage.
/// Keep in sync with `MutationKind`'s `#[serde(rename_all = "snake_case")]` —
/// any future variant must appear in both `kind_as_str` and `kind_from_str`.
// Called from the ScanSessionMutationStorage impl; suppress until the trait
// impl is reached through live code (Task 4 wiring).
#[allow(dead_code)]
fn kind_as_str(k: MutationKind) -> &'static str {
    match k {
        MutationKind::Overwrite => "overwrite",
        MutationKind::Rename => "rename",
        MutationKind::ContainerConversion => "container_conversion",
        MutationKind::NewOutput => "new_output",
    }
}

#[allow(dead_code)]
fn kind_from_str(s: &str) -> Result<MutationKind> {
    match s {
        "overwrite" => Ok(MutationKind::Overwrite),
        "rename" => Ok(MutationKind::Rename),
        "container_conversion" => Ok(MutationKind::ContainerConversion),
        "new_output" => Ok(MutationKind::NewOutput),
        other => Err(VoomError::Storage {
            kind: StorageErrorKind::Other,
            message: format!("scan_session_mutations.kind: unknown MutationKind '{other}'"),
        }),
    }
}

impl ScanSessionMutationStorage for SqliteStore {
    fn record_voom_mutation(&self, m: &VoomOriginatedMutation) -> Result<()> {
        let conn = self.conn()?;
        let original = m.original.as_ref().map(|p| p.to_string_lossy().to_string());
        let recorded_unix = m
            .recorded_at
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(other_storage_err("recorded_at before UNIX_EPOCH"))?
            .as_secs() as i64;
        conn.execute(
            "INSERT OR REPLACE INTO scan_session_mutations \
             (session_id, path, original_path, kind, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                m.session.to_string(),
                m.path.to_string_lossy().to_string(),
                original,
                kind_as_str(m.kind),
                recorded_unix,
            ],
        )
        .map_err(storage_err("insert scan_session_mutation"))?;
        Ok(())
    }

    fn is_voom_originated(&self, session: ScanSessionId, path: &Path) -> Result<bool> {
        let conn = self.conn()?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scan_session_mutations \
                 WHERE session_id = ?1 AND path = ?2",
                params![session.to_string(), path.to_string_lossy().to_string()],
                |r| r.get(0),
            )
            .map_err(storage_err("count scan_session_mutations"))?;
        Ok(n > 0)
    }

    fn voom_mutations_for_session(
        &self,
        session: ScanSessionId,
    ) -> Result<Vec<VoomOriginatedMutation>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT path, original_path, kind, recorded_at \
                 FROM scan_session_mutations WHERE session_id = ?1",
            )
            .map_err(storage_err("prepare scan_session_mutations select"))?;
        let rows = stmt
            .query_map(params![session.to_string()], |row| {
                let path: String = row.get(0)?;
                let original: Option<String> = row.get(1)?;
                let kind: String = row.get(2)?;
                let recorded_unix: i64 = row.get(3)?;
                Ok((path, original, kind, recorded_unix))
            })
            .map_err(storage_err("query scan_session_mutations"))?;
        let mut out = Vec::new();
        for row in rows {
            let (path, original, kind, ts) =
                row.map_err(storage_err("row scan_session_mutations"))?;
            let secs = u64::try_from(ts).map_err(other_storage_err("negative recorded_at"))?;
            let recorded_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            let mut m = VoomOriginatedMutation::new(
                session,
                PathBuf::from(path),
                original.map(PathBuf::from),
                kind_from_str(&kind)?,
            );
            m.recorded_at = recorded_at;
            out.push(m);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use voom_domain::scan_session_mutations::{MutationKind, VoomOriginatedMutation};
    use voom_domain::transition::ScanSessionId;

    use super::ScanSessionMutationStorage;
    use crate::store::SqliteStore;

    fn store() -> SqliteStore {
        SqliteStore::in_memory().expect("open in-memory store")
    }

    #[test]
    fn record_and_lookup_overwrite() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/foo.mkv".into(),
            None,
            MutationKind::Overwrite,
        );
        s.record_voom_mutation(&m).unwrap();
        assert!(
            s.is_voom_originated(session, Path::new("/m/foo.mkv"))
                .unwrap()
        );
        assert!(
            !s.is_voom_originated(session, Path::new("/m/bar.mkv"))
                .unwrap()
        );
    }

    #[test]
    fn record_rename_round_trips_original() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/foo.mp4".into(),
            Some("/m/foo.mkv".into()),
            MutationKind::Rename,
        );
        s.record_voom_mutation(&m).unwrap();
        let all = s.voom_mutations_for_session(session).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].original.as_deref(), Some(Path::new("/m/foo.mkv")));
        assert_eq!(all[0].kind, MutationKind::Rename);
        assert_eq!(all[0].path, std::path::PathBuf::from("/m/foo.mp4"));
    }

    #[test]
    fn other_sessions_do_not_see_each_others_mutations() {
        let s = store();
        let a = ScanSessionId::new();
        let b = ScanSessionId::new();
        s.record_voom_mutation(&VoomOriginatedMutation::new(
            a,
            "/m/foo.mkv".into(),
            None,
            MutationKind::Overwrite,
        ))
        .unwrap();
        assert!(s.is_voom_originated(a, Path::new("/m/foo.mkv")).unwrap());
        assert!(!s.is_voom_originated(b, Path::new("/m/foo.mkv")).unwrap());
    }

    #[test]
    fn record_is_idempotent_via_insert_or_replace() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/foo.mkv".into(),
            None,
            MutationKind::Overwrite,
        );
        s.record_voom_mutation(&m).unwrap();
        s.record_voom_mutation(&m).unwrap();
        let all = s.voom_mutations_for_session(session).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn voom_mutations_for_session_is_empty_for_new_session() {
        let s = store();
        let session = ScanSessionId::new();
        assert!(s.voom_mutations_for_session(session).unwrap().is_empty());
    }

    #[test]
    fn round_trips_container_conversion_kind() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/foo.mp4".into(),
            Some("/m/foo.mkv".into()),
            MutationKind::ContainerConversion,
        );
        s.record_voom_mutation(&m).unwrap();
        let all = s.voom_mutations_for_session(session).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, MutationKind::ContainerConversion);
    }

    #[test]
    fn round_trips_new_output_kind() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/forced.srt".into(),
            None,
            MutationKind::NewOutput,
        );
        s.record_voom_mutation(&m).unwrap();
        let all = s.voom_mutations_for_session(session).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, MutationKind::NewOutput);
    }

    #[test]
    fn voom_mutations_for_session_errors_on_unknown_kind_in_row() {
        let s = store();
        let session = ScanSessionId::new();
        // Insert a row directly with a kind value that does not match any
        // MutationKind variant. This simulates corruption or a future-rolled-back
        // variant on disk.
        let conn = s.conn().expect("conn");
        conn.execute(
            "INSERT INTO scan_session_mutations \
             (session_id, path, original_path, kind, recorded_at) \
             VALUES (?1, ?2, NULL, 'definitely_not_a_real_kind', 0)",
            rusqlite::params![session.to_string(), "/m/foo.mkv"],
        )
        .expect("seed corrupt row");

        let err = s
            .voom_mutations_for_session(session)
            .expect_err("unknown kind must surface as error");
        let msg = err.to_string();
        assert!(
            msg.contains("definitely_not_a_real_kind") || msg.contains("MutationKind"),
            "error message should mention the offending value or variant; got: {msg}"
        );
    }
}
