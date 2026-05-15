//! Discovery scan configuration. Carried on `Call::ScanLibrary`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::errors::Result;
use crate::media::StoredFingerprint;
use crate::scan_session_mutations::SessionMutationSnapshot;

/// Progress update during a scan.
#[derive(Debug, Clone)]
pub enum ScanProgress {
    /// Discovery phase: found a file during directory walk.
    Discovered { count: usize, path: PathBuf },
    /// Processing phase: hashing/building event for a file.
    Processing {
        current: usize,
        total: usize,
        path: PathBuf,
    },
    /// A file's hash was reused from a cached fingerprint — no read took place.
    HashReused { path: PathBuf },
    /// Orphaned voom temp files were found and skipped.
    OrphanedTempFiles { count: usize },
}

/// Callback for files that fail during discovery (path, size, error message).
pub type ErrorCallback = Box<dyn Fn(PathBuf, u64, String) + Send + Sync>;

/// Callback that looks up a previously-stored fingerprint for a given file path.
///
/// Returning `None` forces discovery to compute a fresh content hash. Returning
/// `Some(fingerprint)` allows discovery to skip hashing if the file's size and
/// mtime indicate it has not changed.
pub type FingerprintLookup = Box<dyn Fn(&Path) -> Option<StoredFingerprint> + Send + Sync>;

/// Factory that loads a fresh [`SessionMutationSnapshot`] before each walk.
///
/// Called once at the start of `scan_directory_streaming`. An `Err` result
/// causes the scan to abort (fail-closed).
pub type MutationSnapshotLoader = Arc<dyn Fn() -> Result<SessionMutationSnapshot> + Send + Sync>;

/// Configuration for a discovery scan.
#[non_exhaustive]
pub struct ScanOptions {
    /// Root directory to scan.
    pub root: PathBuf,
    /// Whether to recurse into subdirectories.
    pub recursive: bool,
    /// Whether to compute content hashes (xxHash64).
    pub hash_files: bool,
    /// Number of parallel workers for hashing (0 = auto).
    pub workers: usize,
    /// Optional progress callback.
    pub on_progress: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
    /// Optional error callback for files that fail during discovery
    /// (e.g., disappeared between walk and hash). Called with (path, size, `error_message`).
    /// Size is captured during the directory walk and may be stale if the file changed.
    pub on_error: Option<ErrorCallback>,
    /// Optional fingerprint lookup. When set, discovery reuses the cached
    /// `content_hash` for files whose on-disk `size` and `mtime` indicate no
    /// change, avoiding a potentially expensive re-read.
    ///
    /// Has no effect when `hash_files` is `false`.
    pub fingerprint_lookup: Option<FingerprintLookup>,
    /// Optional loader for the session mutation snapshot.
    ///
    /// When set, called once before the walk begins. Returns the set of paths
    /// that VOOM has already mutated this session; these paths are excluded from
    /// discovery. If the loader returns an error the scan aborts (fail-closed).
    pub session_mutations: Option<MutationSnapshotLoader>,
}

impl ScanOptions {
    /// Construct with a root path.
    ///
    /// Defaults must mirror the previous home in `plugins/discovery/src/lib.rs`
    /// exactly (rev-3): `recursive: true`, `hash_files: true`, `workers: 0` (auto).
    /// Silently changing `hash_files` to `false` would regress fingerprint reuse
    /// and duplicate detection.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            recursive: true,
            hash_files: true,
            workers: 0,
            on_progress: None,
            on_error: None,
            fingerprint_lookup: None,
            session_mutations: None,
        }
    }
}

impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("root", &self.root)
            .field("recursive", &self.recursive)
            .field("hash_files", &self.hash_files)
            .field("workers", &self.workers)
            .field("on_progress", &self.on_progress.as_ref().map(|_| "..."))
            .field("on_error", &self.on_error.as_ref().map(|_| "..."))
            .field(
                "fingerprint_lookup",
                &self.fingerprint_lookup.as_ref().map(|_| "..."),
            )
            .field(
                "session_mutations",
                &self.session_mutations.as_ref().map(|_| "..."),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_preserves_existing_defaults() {
        let opts = ScanOptions::new(PathBuf::from("/tmp"));
        assert_eq!(opts.root, PathBuf::from("/tmp"));
        assert!(opts.recursive, "recursive default must remain true");
        assert!(
            opts.hash_files,
            "hash_files default must remain true (or fingerprint reuse silently breaks)"
        );
        assert_eq!(opts.workers, 0, "workers default must remain 0 (auto)");
        assert!(opts.on_progress.is_none());
        assert!(opts.on_error.is_none());
        assert!(opts.fingerprint_lookup.is_none());
        assert!(opts.session_mutations.is_none());
    }
}
