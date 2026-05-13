//! Storage for `VoomOriginatedMutation` records, keyed by `(session, path)`.
//!
//! These rows let the scanner and `finish_scan_session` distinguish VOOM's own
//! filesystem writes from external changes.

use std::path::Path;

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
    /// Returns one record per VOOM-touched path in the session. Rename pairs
    /// are decomposed at the storage boundary, so callers see two separate
    /// records (one for the source path, one for the destination) rather than
    /// a single record with an `original` field. This makes the on-disk model
    /// safe against same-destination re-entrant writes.
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
        let mut conn = self.conn()?;
        let recorded_unix = i64::try_from(
            m.recorded_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(other_storage_err("recorded_at before UNIX_EPOCH"))?
                .as_secs(),
        )
        .map_err(other_storage_err("recorded_at exceeds i64"))?;
        let kind = kind_as_str(m.kind);

        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage_err("begin tx for record_voom_mutation"))?;

        // One row per touched path. Same-destination re-entry is harmless because
        // INSERT OR REPLACE keeps the row present in the skip set; the kind and
        // recorded_at fields update but the path's protection is invariant.
        tx.execute(
            "INSERT OR REPLACE INTO scan_session_mutations \
             (session_id, path, kind, recorded_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                m.session.to_string(),
                m.path.to_string_lossy().to_string(),
                kind,
                recorded_unix,
            ],
        )
        .map_err(storage_err("insert destination row"))?;

        if let Some(orig) = m.original.as_ref() {
            tx.execute(
                "INSERT OR REPLACE INTO scan_session_mutations \
                 (session_id, path, kind, recorded_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    m.session.to_string(),
                    orig.to_string_lossy().to_string(),
                    kind,
                    recorded_unix,
                ],
            )
            .map_err(storage_err("insert source row"))?;
        }

        tx.commit()
            .map_err(storage_err("commit record_voom_mutation"))?;
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
                "SELECT path, kind, recorded_at \
                 FROM scan_session_mutations WHERE session_id = ?1",
            )
            .map_err(storage_err("prepare scan_session_mutations select"))?;
        let rows = stmt
            .query_map(params![session.to_string()], |row| {
                let path: String = row.get(0)?;
                let kind: String = row.get(1)?;
                let recorded_unix: i64 = row.get(2)?;
                Ok((path, kind, recorded_unix))
            })
            .map_err(storage_err("query scan_session_mutations"))?;
        let mut out = Vec::new();
        for row in rows {
            let (path, kind, ts) = row.map_err(storage_err("row scan_session_mutations"))?;
            let ts_u64 = u64::try_from(ts).map_err(other_storage_err("negative recorded_at"))?;
            let mut m =
                VoomOriginatedMutation::new(session, path.into(), None, kind_from_str(&kind)?);
            m.recorded_at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts_u64);
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
    fn rename_records_both_source_and_destination() {
        let s = store();
        let session = ScanSessionId::new();
        let m = VoomOriginatedMutation::new(
            session,
            "/m/foo.mp4".into(),
            Some("/m/foo.mkv".into()),
            MutationKind::Rename,
        );
        s.record_voom_mutation(&m).unwrap();

        let mut all = s.voom_mutations_for_session(session).unwrap();
        all.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(all.len(), 2, "rename must produce one row per touched path");
        assert_eq!(all[0].path, std::path::PathBuf::from("/m/foo.mkv"));
        assert_eq!(all[1].path, std::path::PathBuf::from("/m/foo.mp4"));
        for m in &all {
            assert_eq!(m.kind, MutationKind::Rename);
            assert!(
                m.original.is_none(),
                "reconstructed records carry no original"
            );
        }
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
        let mut all = s.voom_mutations_for_session(session).unwrap();
        // source + destination each get their own row
        assert_eq!(all.len(), 2);
        for row in &all {
            assert_eq!(row.kind, MutationKind::ContainerConversion);
        }
        all.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(all[0].path, std::path::PathBuf::from("/m/foo.mkv"));
        assert_eq!(all[1].path, std::path::PathBuf::from("/m/foo.mp4"));
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
             (session_id, path, kind, recorded_at) \
             VALUES (?1, ?2, 'definitely_not_a_real_kind', 0)",
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

    #[test]
    fn reentrant_destination_write_keeps_earlier_source_protected() {
        let s = store();
        let session = ScanSessionId::new();

        // First: VOOM renames A -> B.
        s.record_voom_mutation(&VoomOriginatedMutation::new(
            session,
            "/m/B.mp4".into(),
            Some("/m/A.mkv".into()),
            MutationKind::Rename,
        ))
        .unwrap();

        // Then: VOOM overwrites B again (e.g., post-rename muxer pass).
        s.record_voom_mutation(&VoomOriginatedMutation::new(
            session,
            "/m/B.mp4".into(),
            None,
            MutationKind::Overwrite,
        ))
        .unwrap();

        // Both A and B must still be in the protected set. With the old
        // INSERT-OR-REPLACE-on-row schema, A would be evicted by the second
        // write and finish_scan_session would mark A missing.
        assert!(
            s.is_voom_originated(session, std::path::Path::new("/m/A.mkv"))
                .unwrap(),
            "rename source must survive a later same-destination write"
        );
        assert!(
            s.is_voom_originated(session, std::path::Path::new("/m/B.mp4"))
                .unwrap(),
            "destination must still be protected"
        );
    }
}
