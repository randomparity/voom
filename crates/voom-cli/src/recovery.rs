//! Crash recovery: detect and resolve orphaned `.vbak` backup files.
//!
//! After a crash or hard kill, `.vbak` files written by the backup-manager
//! may be left on disk with no corresponding completion record. This module
//! scans for them and resolves each one according to the `recovery.mode`
//! config setting.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::RecoveryConfig;
use voom_domain::transition::TransitionSource;
use voom_domain::FileTransition;

/// An orphaned backup file found on disk with no corresponding completion.
#[derive(Debug)]
pub struct OrphanedBackup {
    /// Path to the `.vbak` file.
    pub backup_path: PathBuf,
    /// Inferred original file path.
    pub original_path: PathBuf,
    /// Size of the backup file.
    pub size: u64,
}

/// Scan `scan_dirs` for orphaned `.vbak` files and resolve them per `config`.
///
/// Returns the number of files successfully resolved (restored or discarded).
pub fn check_and_recover_under(
    config: &RecoveryConfig,
    scan_dirs: &[PathBuf],
    store: &dyn voom_domain::storage::StorageTrait,
) -> Result<u64> {
    let pending = store.list_pending_ops().unwrap_or_default();
    let all_backups = find_orphans_under(scan_dirs)?;

    if pending.is_empty() && all_backups.is_empty() {
        return Ok(0);
    }

    // Build set of file paths with pending operations.
    let pending_paths: std::collections::HashSet<String> = pending
        .iter()
        .map(|op| op.file_path.to_string_lossy().to_string())
        .collect();

    // A backup is an orphan if its original path has a pending operation.
    let orphans: Vec<_> = all_backups
        .into_iter()
        .filter(|b| {
            let path_str = b.original_path.to_string_lossy().to_string();
            pending_paths.contains(&path_str)
        })
        .collect();

    if orphans.is_empty() {
        if !pending.is_empty() {
            // Stale pending ops with no backups — clean them up.
            for op in &pending {
                tracing::warn!(
                    plan_id = %op.id,
                    path = %op.file_path.display(),
                    "stale pending operation with no backup — removing"
                );
                let _ = store.delete_pending_op(&op.id);
            }
        }
        return Ok(0);
    }

    tracing::info!(
        count = orphans.len(),
        "found orphaned backup files from crashed executions"
    );

    let mut resolved = 0u64;
    for orphan in &orphans {
        let result = match config.mode.as_str() {
            "always_recover" => recover(orphan, store),
            "always_discard" => discard(orphan, store),
            _ => {
                tracing::warn!(
                    backup = %orphan.backup_path.display(),
                    "orphaned backup found — set recovery.mode in config.toml"
                );
                continue;
            }
        };
        match result {
            Ok(()) => {
                resolved += 1;
                // Clean up the pending operation row(s) for this path.
                let path_str = orphan.original_path.to_string_lossy().to_string();
                for op in pending
                    .iter()
                    .filter(|op| *op.file_path.to_string_lossy() == path_str)
                {
                    let _ = store.delete_pending_op(&op.id);
                }
            }
            Err(e) => tracing::warn!(
                backup = %orphan.backup_path.display(),
                error = %e,
                "failed to resolve orphaned backup"
            ),
        }
    }
    Ok(resolved)
}

/// Walk each directory looking for `.vbak` files inside `.voom-backup/` subdirectories.
///
/// Returns all backup files found; callers cross-reference with `pending_operations`
/// to determine which are genuine orphans from crashed executions.
fn find_orphans_under(dirs: &[PathBuf]) -> Result<Vec<OrphanedBackup>> {
    let mut backups = Vec::new();

    for dir in dirs {
        collect_orphans_in(dir, &mut backups);
    }

    Ok(backups)
}

/// Recursively collect orphaned `.vbak` files under `dir` using `std::fs::read_dir`.
fn collect_orphans_in(dir: &Path, orphans: &mut Vec<OrphanedBackup>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(path = %dir.display(), error = %e, "cannot read dir during recovery scan");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if meta.is_dir() {
            let dir_name = entry.file_name();
            if dir_name == ".voom-backup" {
                // Scan this backup dir for .vbak files (one level deep only)
                collect_vbak_files(&path, orphans);
            } else {
                // Recurse into other directories
                collect_orphans_in(&path, orphans);
            }
        }
    }
}

/// Collect `.vbak` files directly inside a `.voom-backup/` directory.
fn collect_vbak_files(backup_dir: &Path, orphans: &mut Vec<OrphanedBackup>) {
    let entries = match std::fs::read_dir(backup_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                path = %backup_dir.display(),
                error = %e,
                "cannot read .voom-backup dir"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "vbak") {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if let Some(original) = infer_original_path(&path) {
                orphans.push(OrphanedBackup {
                    backup_path: path,
                    original_path: original,
                    size,
                });
            } else {
                tracing::warn!(
                    path = %path.display(),
                    "could not infer original path from backup filename, skipping"
                );
            }
        }
    }
}

/// Derive the original file path from a sibling-mode backup path.
///
/// Sibling backup format: `<parent>/.voom-backup/<stem>.<YYYYmmddHHMMSS>.vbak`
/// Original:              `<parent>/<stem>`
///
/// Strips the `.voom-backup` directory component and the `.<14-digit-timestamp>.vbak`
/// suffix to recover the original filename, then reconstructs the original path.
fn infer_original_path(backup_path: &Path) -> Option<PathBuf> {
    // Parent of the backup file should be the `.voom-backup` dir.
    // Parent of that is the original file's parent dir.
    let backup_dir = backup_path.parent()?;
    let original_dir = backup_dir.parent()?;

    let backup_filename = backup_path.file_name()?.to_string_lossy();

    // Strip the `.vbak` extension.
    let without_ext = backup_filename.strip_suffix(".vbak")?;

    // Strip the `.<14-digit-timestamp>` suffix: a literal dot followed by 14 ASCII digits.
    let original_filename = strip_timestamp_suffix(without_ext)?;

    // Normalize the parent directory (which exists on disk) separately,
    // then join the filename. The original file itself may not exist
    // (that's why we're doing recovery), so normalizing the full path
    // would fall back to the raw non-canonical path.
    let normalized_dir = voom_discovery::normalize_path(original_dir);
    Some(normalized_dir.join(original_filename))
}

/// Strip a trailing `.<14-digit-timestamp>` from a filename stem.
///
/// Returns `None` if the suffix is not present (file doesn't match expected format).
fn strip_timestamp_suffix(name: &str) -> Option<&str> {
    // Pattern: ends with ".<14 digits>"
    let dot_pos = name.rfind('.')?;
    let suffix = &name[dot_pos + 1..];
    if suffix.len() == 14 && suffix.bytes().all(|b| b.is_ascii_digit()) {
        Some(&name[..dot_pos])
    } else {
        None
    }
}

/// Restore the original file from its backup and record the transition.
fn recover(orphan: &OrphanedBackup, store: &dyn voom_domain::storage::StorageTrait) -> Result<()> {
    voom_backup_manager::backup::restore_from_paths(&orphan.backup_path, &orphan.original_path)
        .with_context(|| {
            format!(
                "restore {} from {}",
                orphan.original_path.display(),
                orphan.backup_path.display()
            )
        })?;

    // Hash the restored file so the transition record is accurate.
    let to_hash = match voom_discovery::hash_file(&orphan.original_path) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                path = %orphan.original_path.display(),
                error = %e,
                "could not hash restored file, using empty hash"
            );
            String::new()
        }
    };
    let to_size = std::fs::metadata(&orphan.original_path)
        .map(|m| m.len())
        .unwrap_or(orphan.size);

    record_recovery_transition(
        store,
        &orphan.original_path,
        to_hash,
        to_size,
        "crash_recovery:restored",
    );

    // Remove the backup file after successful restore.
    voom_backup_manager::backup::remove_vbak_file(&orphan.backup_path)
        .with_context(|| format!("remove backup {}", orphan.backup_path.display()))?;

    tracing::info!(
        original = %orphan.original_path.display(),
        backup = %orphan.backup_path.display(),
        "crash recovery: restored file from backup"
    );

    Ok(())
}

/// Delete the backup file without restoring, accepting the current on-disk state.
fn discard(orphan: &OrphanedBackup, store: &dyn voom_domain::storage::StorageTrait) -> Result<()> {
    voom_backup_manager::backup::remove_vbak_file(&orphan.backup_path)
        .with_context(|| format!("remove backup {}", orphan.backup_path.display()))?;

    // Hash the current on-disk file (if it exists) for the transition record.
    let (to_hash, to_size) = if orphan.original_path.exists() {
        let hash = voom_discovery::hash_file(&orphan.original_path).unwrap_or_default();
        let size = std::fs::metadata(&orphan.original_path)
            .map(|m| m.len())
            .unwrap_or(0);
        (hash, size)
    } else {
        (String::new(), 0)
    };

    record_recovery_transition(
        store,
        &orphan.original_path,
        to_hash,
        to_size,
        "crash_recovery:discarded",
    );

    tracing::info!(
        original = %orphan.original_path.display(),
        backup = %orphan.backup_path.display(),
        "crash recovery: discarded backup, keeping current on-disk state"
    );

    Ok(())
}

/// Record an Unknown-source transition for recovery actions, best-effort.
///
/// Silently skips if the file is not in the database (never recorded by voom).
fn record_recovery_transition(
    store: &dyn voom_domain::storage::StorageTrait,
    original_path: &Path,
    to_hash: String,
    to_size: u64,
    detail: &str,
) {
    let file = match store.file_by_path(original_path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            tracing::debug!(
                path = %original_path.display(),
                "file not in database, skipping transition record"
            );
            return;
        }
        Err(e) => {
            tracing::warn!(
                path = %original_path.display(),
                error = %e,
                "could not look up file for transition record"
            );
            return;
        }
    };

    let transition = FileTransition::new(
        file.id,
        original_path.to_path_buf(),
        to_hash,
        to_size,
        TransitionSource::Unknown,
    )
    .with_detail(detail);

    if let Err(e) = store.record_transition(&transition) {
        tracing::warn!(
            path = %original_path.display(),
            error = %e,
            "failed to record crash recovery transition"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── infer_original_path ──────────────────────────────────────────────────

    #[test]
    fn test_infer_original_path_sibling_mode() {
        // normalize_path falls back to raw path for non-existent files
        let backup = PathBuf::from("/media/movies/.voom-backup/Movie.mkv.20240315120000.vbak");
        let original = infer_original_path(&backup).unwrap();
        assert_eq!(original, PathBuf::from("/media/movies/Movie.mkv"));
    }

    #[test]
    fn test_infer_original_path_nested() {
        let backup = PathBuf::from("/a/b/c/.voom-backup/show.s01e01.mkv.20240101000000.vbak");
        let original = infer_original_path(&backup).unwrap();
        assert_eq!(original, PathBuf::from("/a/b/c/show.s01e01.mkv"));
    }

    #[test]
    fn test_infer_original_path_non_matching_suffix_returns_none() {
        let backup = PathBuf::from("/media/.voom-backup/Movie.mkv.notvalid.vbak");
        assert!(infer_original_path(&backup).is_none());
    }

    #[test]
    fn test_infer_original_path_wrong_digit_count_returns_none() {
        let backup = PathBuf::from("/media/.voom-backup/Movie.mkv.202403151200.vbak");
        assert!(infer_original_path(&backup).is_none());
    }

    #[test]
    fn test_strip_timestamp_suffix_valid() {
        assert_eq!(
            strip_timestamp_suffix("Movie.mkv.20240315120000"),
            Some("Movie.mkv")
        );
    }

    #[test]
    fn test_strip_timestamp_suffix_no_dot() {
        assert_eq!(strip_timestamp_suffix("nodothere"), None);
    }

    #[test]
    fn test_strip_timestamp_suffix_wrong_length() {
        assert_eq!(strip_timestamp_suffix("Movie.mkv.202403"), None);
    }

    // ── find_orphans_under ───────────────────────────────────────────────────

    #[test]
    fn test_find_orphans_detects_vbak_in_voom_backup_dir() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = std::fs::canonicalize(dir.path()).unwrap();
        let backup_dir = real_dir.join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let vbak = backup_dir.join("test.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"backup content").unwrap();

        let orphans = find_orphans_under(&[real_dir.clone()]).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].backup_path, vbak);
        assert_eq!(orphans[0].original_path, real_dir.join("test.mkv"));
        assert_eq!(orphans[0].size, 14); // "backup content"
    }

    #[test]
    fn test_find_orphans_ignores_vbak_outside_voom_backup_dir() {
        let dir = tempfile::tempdir().unwrap();

        let vbak = dir.path().join("stray.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"stray").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert!(orphans.is_empty(), "stray .vbak should be ignored");
    }

    #[test]
    fn test_find_orphans_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = std::fs::canonicalize(dir.path()).unwrap();
        let sub = real_dir.join("Season 1");
        let backup_dir = sub.join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let vbak = backup_dir.join("ep01.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"data").unwrap();

        let orphans = find_orphans_under(&[real_dir]).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].original_path, sub.join("ep01.mkv"));
    }

    #[test]
    fn test_find_orphans_empty_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn test_find_orphans_multiple_dirs() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        let backup_dir1 = dir1.path().join(".voom-backup");
        let backup_dir2 = dir2.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir1).unwrap();
        std::fs::create_dir_all(&backup_dir2).unwrap();

        std::fs::write(backup_dir1.join("a.mkv.20240315120000.vbak"), b"a").unwrap();
        std::fs::write(backup_dir2.join("b.mkv.20240315120001.vbak"), b"b").unwrap();

        let orphans =
            find_orphans_under(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]).unwrap();
        assert_eq!(orphans.len(), 2);
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_backup(dir: &std::path::Path, filename: &str) -> (PathBuf, PathBuf) {
        let backup_dir = dir.join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join(format!("{filename}.20240315120000.vbak"));
        std::fs::write(&vbak, b"backup content").unwrap();
        // Return normalized original path (matching what infer_original_path
        // produces after normalize_path canonicalizes).
        let canonical_dir = std::fs::canonicalize(dir).unwrap();
        let original = canonical_dir.join(filename);
        (vbak, original)
    }

    fn insert_pending_op(
        store: &voom_domain::test_support::InMemoryStore,
        original_path: &std::path::Path,
    ) {
        use voom_domain::storage::{PendingOperation, PendingOpsStorage};
        let op = PendingOperation {
            id: uuid::Uuid::new_v4(),
            file_path: original_path.to_path_buf(),
            phase_name: "test".into(),
            started_at: chrono::Utc::now(),
        };
        store.insert_pending_op(&op).unwrap();
    }

    // ── orphan detection via pending_operations ─────────────────────────────

    #[test]
    fn test_orphan_detected_when_pending_op_exists() {
        let dir = tempfile::tempdir().unwrap();
        let (_vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1, "backup with pending op should be recovered");
    }

    #[test]
    fn test_no_orphan_when_no_pending_op() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, _original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        // No pending op inserted

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 0);
        assert!(
            vbak.exists(),
            "backup with no pending op should not be touched"
        );
    }

    #[test]
    fn test_no_orphan_when_no_backup() {
        use voom_domain::storage::PendingOpsStorage as _;

        let dir = tempfile::tempdir().unwrap();
        // No backup file created, just a pending op
        let original = dir.path().join("movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 0);
        // Stale pending op should have been cleaned up
        let remaining = store.list_pending_ops().unwrap();
        assert!(remaining.is_empty(), "stale pending op should be removed");
    }

    // ── check_and_recover_under modes ────────────────────────────────────────

    #[test]
    fn test_check_and_recover_always_recover_restores_file() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1);
        let content = std::fs::read(&original).unwrap();
        assert_eq!(content, b"backup content");
        assert!(!vbak.exists(), "backup should be removed after restore");
    }

    #[test]
    fn test_check_and_recover_always_discard_removes_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "always_discard".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1);
        assert!(
            !original.exists(),
            "original should not exist in discard mode"
        );
        assert!(!vbak.exists(), "backup should be discarded");
    }

    #[test]
    fn test_check_and_recover_skips_backup_with_no_pending_op() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, _original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        // No pending op — a completed execution would have deleted it

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 0);
        assert!(
            vbak.exists(),
            "backup with no pending op should not be removed"
        );
    }

    #[test]
    fn test_check_and_recover_prompt_skips_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "prompt".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 0);
        assert!(vbak.exists());
    }

    #[test]
    fn test_check_and_recover_cleans_pending_ops_after_resolve() {
        use voom_domain::storage::PendingOpsStorage as _;

        let dir = tempfile::tempdir().unwrap();
        let (_vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_pending_op(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        let remaining = store.list_pending_ops().unwrap();
        assert!(
            remaining.is_empty(),
            "pending op should be deleted after recovery"
        );
    }
}
