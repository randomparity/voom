//! Records describing filesystem mutations originated by VOOM itself within
//! the scope of a single scan session. Used by the storage layer to skip
//! VOOM-touched paths during `finish_scan_session` reconciliation and by the
//! scanner to avoid re-discovering its own outputs.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::transition::ScanSessionId;

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
}
