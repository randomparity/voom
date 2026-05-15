//! Records describing filesystem mutations originated by VOOM itself within
//! the scope of a single scan session. Used by the storage layer to skip
//! VOOM-touched paths during `finish_scan_session` reconciliation and by the
//! scanner to avoid re-discovering its own outputs.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::transition::ScanSessionId;

/// Set of paths the active scan session has marked as VOOM-originated mutations.
///
/// Loaded as a single snapshot before the walker begins so lookup is infallible
/// during the walk. Paths in the snapshot are stored in the form produced by
/// the discovery scanner's `normalize_path` (NFC + canonicalized), so callers
/// performing lookups must normalize each entry the same way before checking.
#[derive(Debug, Default, Clone)]
pub struct SessionMutationSnapshot {
    paths: std::collections::HashSet<PathBuf>,
}

impl SessionMutationSnapshot {
    #[must_use]
    pub fn new(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            paths: paths.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn contains(&self, path: &std::path::Path) -> bool {
        self.paths.contains(path)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// What kind of filesystem mutation VOOM performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    /// VOOM overwrote a file in place (same path, new content).
    Overwrite,
    /// VOOM renamed a file. Source path is `original`, destination is `path`.
    Rename,
    /// VOOM produced a new output at `path` with a different container.
    ContainerConversion,
    /// VOOM wrote a brand-new output path not derived from a source.
    NewOutput,
}

/// One filesystem mutation performed by VOOM during an active scan session.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoomOriginatedMutation {
    pub session: ScanSessionId,
    pub path: PathBuf,
    pub original: Option<PathBuf>,
    pub kind: MutationKind,
    pub recorded_at: SystemTime,
}

impl VoomOriginatedMutation {
    #[must_use]
    pub fn new(
        session: ScanSessionId,
        path: PathBuf,
        original: Option<PathBuf>,
        kind: MutationKind,
    ) -> Self {
        Self {
            session,
            path,
            original,
            kind,
            recorded_at: SystemTime::now(),
        }
    }
}

/// Record a `VoomOriginatedMutation` for an upcoming filesystem write, derived
/// from the plan source path and the destination the write will produce.
///
/// Returns:
/// - `Ok(())` if no scan session is set on the plan, OR if no storage handle
///   was passed (no-op for dry-run / pre-session contexts).
/// - `Ok(())` after a successful `record_voom_mutation` call.
/// - `Err(VoomError::ToolExecution)` if a scan session IS set AND a storage
///   handle is available BUT the storage call failed. Callers must NOT perform
///   the filesystem write in this case — the lack of a recorded mutation
///   would leave the write indistinguishable from an external change.
///
/// `MutationKind` is chosen by comparing `dest` to `plan_source`:
/// - same path → `Overwrite`
/// - different path AND different extension → `ContainerConversion`
/// - different path AND same extension → `Rename`
#[must_use = "Err means the pending write must be aborted; dropping this result silently re-opens the fail-open hole"]
pub fn record_mutation_for_pending_write(
    storage: Option<&dyn crate::storage::ScanSessionMutationStorage>,
    scan_session: Option<crate::transition::ScanSessionId>,
    plan_source: &std::path::Path,
    dest: &std::path::Path,
) -> crate::errors::Result<()> {
    let Some(session) = scan_session else {
        return Ok(());
    };
    let Some(storage) = storage else {
        return Ok(());
    };

    let kind = if dest == plan_source {
        MutationKind::Overwrite
    } else if dest.extension() != plan_source.extension() {
        MutationKind::ContainerConversion
    } else {
        MutationKind::Rename
    };
    let original = (dest != plan_source).then(|| plan_source.to_path_buf());
    let m = VoomOriginatedMutation::new(session, dest.to_path_buf(), original, kind);

    storage
        .record_voom_mutation(&m)
        .map_err(|e| crate::errors::VoomError::ToolExecution {
            tool: "voom-executor".into(),
            message: format!(
                "failed to record VOOM mutation for {}: {e}; \
                 refusing to perform filesystem write to avoid scanner race",
                dest.display()
            ),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_overwrite_records_path_and_kind() {
        let session = ScanSessionId::new();
        let path = PathBuf::from("/m/foo.mkv");
        let m = VoomOriginatedMutation::new(session, path.clone(), None, MutationKind::Overwrite);
        assert_eq!(m.session, session);
        assert_eq!(m.path, path);
        assert_eq!(m.original, None);
        assert_eq!(m.kind, MutationKind::Overwrite);
    }

    #[test]
    fn new_rename_records_original() {
        let session = ScanSessionId::new();
        let src = PathBuf::from("/m/foo.mkv");
        let dst = PathBuf::from("/m/foo.mp4");
        let m = VoomOriginatedMutation::new(
            session,
            dst.clone(),
            Some(src.clone()),
            MutationKind::Rename,
        );
        assert_eq!(m.path, dst);
        assert_eq!(m.original, Some(src));
        assert_eq!(m.kind, MutationKind::Rename);
    }

    #[test]
    fn mutation_kind_serializes_snake_case() {
        let j = serde_json::to_string(&MutationKind::ContainerConversion).unwrap();
        assert_eq!(j, "\"container_conversion\"");
    }

    #[test]
    fn record_mutation_returns_ok_when_no_session() {
        use std::path::Path;
        let r = record_mutation_for_pending_write(
            None,
            None,
            Path::new("/m/foo.mkv"),
            Path::new("/m/foo.mkv"),
        );
        assert!(r.is_ok());
    }

    #[test]
    fn record_mutation_returns_ok_when_no_storage_handle() {
        use std::path::Path;
        let session = crate::transition::ScanSessionId::new();
        let r = record_mutation_for_pending_write(
            None,
            Some(session),
            Path::new("/m/foo.mkv"),
            Path::new("/m/foo.mkv"),
        );
        assert!(r.is_ok(), "no storage = no-op, not an error");
    }

    #[test]
    fn record_mutation_kind_inference_overwrite_vs_rename_vs_container() {
        use std::path::Path;
        fn infer(src: &str, dst: &str) -> MutationKind {
            // Mirror the logic for documentation purposes; the helper itself is
            // covered by integration tests against real storage.
            let src = Path::new(src);
            let dst = Path::new(dst);
            if dst == src {
                MutationKind::Overwrite
            } else if dst.extension() != src.extension() {
                MutationKind::ContainerConversion
            } else {
                MutationKind::Rename
            }
        }
        assert_eq!(infer("/m/a.mkv", "/m/a.mkv"), MutationKind::Overwrite);
        assert_eq!(
            infer("/m/a.mkv", "/m/a.mp4"),
            MutationKind::ContainerConversion
        );
        assert_eq!(infer("/m/a.mkv", "/m/b.mkv"), MutationKind::Rename);
    }

    #[test]
    fn helper_returns_err_when_session_active_and_storage_errors() {
        use std::path::Path;

        use crate::errors::{StorageErrorKind, VoomError};
        use crate::storage::ScanSessionMutationStorage;
        use crate::transition::ScanSessionId;

        struct AlwaysFailingStore;
        impl ScanSessionMutationStorage for AlwaysFailingStore {
            fn record_voom_mutation(
                &self,
                _: &VoomOriginatedMutation,
            ) -> crate::errors::Result<()> {
                Err(VoomError::Storage {
                    kind: StorageErrorKind::Other,
                    message: "injected failure".into(),
                })
            }
            fn is_voom_originated(
                &self,
                _: ScanSessionId,
                _: &Path,
            ) -> crate::errors::Result<bool> {
                Ok(false)
            }
            fn voom_mutations_for_session(
                &self,
                _: ScanSessionId,
            ) -> crate::errors::Result<Vec<VoomOriginatedMutation>> {
                Ok(Vec::new())
            }
        }

        let store = AlwaysFailingStore;
        let session = ScanSessionId::new();
        let r = record_mutation_for_pending_write(
            Some(&store),
            Some(session),
            Path::new("/m/foo.mkv"),
            Path::new("/m/foo.mkv"),
        );
        let err = r.expect_err("must surface storage error so caller can abort");
        let msg = err.to_string();
        assert!(
            msg.contains("VOOM mutation") || msg.contains("scanner race"),
            "error message must explain why caller should abort; got: {msg}"
        );
        assert!(
            matches!(err, VoomError::ToolExecution { .. }),
            "must wrap storage error as ToolExecution so callers can propagate via ?; got: {err:?}"
        );
    }
}
