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
    let orphans = find_orphans_under(scan_dirs)?;
    if orphans.is_empty() {
        return Ok(0);
    }

    tracing::info!(count = orphans.len(), "found orphaned backup files");

    let mut resolved = 0u64;
    for orphan in &orphans {
        match config.mode.as_str() {
            "always_recover" => match recover(orphan, store) {
                Ok(()) => resolved += 1,
                Err(e) => tracing::warn!(
                    backup = %orphan.backup_path.display(),
                    error = %e,
                    "failed to recover from backup"
                ),
            },
            "always_discard" => match discard(orphan, store) {
                Ok(()) => resolved += 1,
                Err(e) => tracing::warn!(
                    backup = %orphan.backup_path.display(),
                    error = %e,
                    "failed to discard backup"
                ),
            },
            _ => {
                // "prompt" or unknown — log and skip in non-interactive mode
                tracing::warn!(
                    backup = %orphan.backup_path.display(),
                    "orphaned backup found — set recovery.mode in config.toml or run interactively"
                );
            }
        }
    }
    Ok(resolved)
}

/// Walk each directory looking for `.vbak` files inside `.voom-backup/` subdirectories.
fn find_orphans_under(dirs: &[PathBuf]) -> Result<Vec<OrphanedBackup>> {
    let mut orphans = Vec::new();

    for dir in dirs {
        collect_orphans_in(dir, &mut orphans);
    }

    // Only treat backups as orphans when the original file is missing or empty.
    // If the original exists with content, the execution likely completed
    // successfully and keep_backups retained the copy — not a crash orphan.
    orphans.retain(|o| match std::fs::metadata(&o.original_path) {
        Ok(meta) if meta.len() > 0 => {
            tracing::debug!(
                backup = %o.backup_path.display(),
                original = %o.original_path.display(),
                "skipping backup — original file exists (likely retained by keep_backups)"
            );
            false
        }
        _ => true,
    });

    Ok(orphans)
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

    Some(original_dir.join(original_filename))
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
        // Suffix has letters, not 14 digits
        let backup = PathBuf::from("/media/.voom-backup/Movie.mkv.notvalid.vbak");
        assert!(infer_original_path(&backup).is_none());
    }

    #[test]
    fn test_infer_original_path_wrong_digit_count_returns_none() {
        // Only 12 digits (too short)
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
        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let vbak = backup_dir.join("test.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"backup content").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].backup_path, vbak);
        assert_eq!(orphans[0].original_path, dir.path().join("test.mkv"));
        assert_eq!(orphans[0].size, 14); // "backup content"
    }

    #[test]
    fn test_find_orphans_ignores_vbak_outside_voom_backup_dir() {
        let dir = tempfile::tempdir().unwrap();

        // A .vbak file not inside a .voom-backup directory — must be ignored.
        let vbak = dir.path().join("stray.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"stray").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert!(orphans.is_empty(), "stray .vbak should be ignored");
    }

    #[test]
    fn test_find_orphans_recurses_into_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Season 1");
        let backup_dir = sub.join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let vbak = backup_dir.join("ep01.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"data").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
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

    // ── check_and_recover_under modes ────────────────────────────────────────

    #[test]
    fn test_check_and_recover_always_recover_restores_file() {
        let dir = tempfile::tempdir().unwrap();

        // Original file is missing (crash before output was written)
        let original = dir.path().join("movie.mkv");

        // Create a backup with the pre-crash content
        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join("movie.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"original content").unwrap();

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };

        let store = voom_domain::test_support::InMemoryStore::default();
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1);
        // Original should now have the backup content
        let content = std::fs::read(&original).unwrap();
        assert_eq!(content, b"original content");
        // Backup file should be gone
        assert!(!vbak.exists(), "backup should be removed after restore");
    }

    #[test]
    fn test_check_and_recover_always_discard_removes_backup() {
        let dir = tempfile::tempdir().unwrap();

        // Original file is missing (crash scenario)
        let original = dir.path().join("movie.mkv");

        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join("movie.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"old backup").unwrap();

        let config = RecoveryConfig {
            mode: "always_discard".into(),
        };

        let store = voom_domain::test_support::InMemoryStore::default();
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1);
        // Original should not exist (was never written, and discard doesn't restore)
        assert!(
            !original.exists(),
            "original should not exist in discard mode"
        );
        // Backup should be removed
        assert!(!vbak.exists(), "backup should be discarded");
    }

    #[test]
    fn test_find_orphans_skips_backup_when_original_exists() {
        let dir = tempfile::tempdir().unwrap();

        // Original file exists with content (successful execution with keep_backups)
        let original = dir.path().join("movie.mkv");
        std::fs::write(&original, b"processed content").unwrap();

        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let _vbak = backup_dir.join("movie.mkv.20240315120000.vbak");
        std::fs::write(&_vbak, b"pre-processing backup").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert!(
            orphans.is_empty(),
            "backup should not be treated as orphan when original exists with content"
        );
    }

    #[test]
    fn test_find_orphans_detects_backup_when_original_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        // Original file exists but is empty (crash during write)
        let original = dir.path().join("movie.mkv");
        std::fs::write(&original, b"").unwrap();

        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join("movie.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"backup content").unwrap();

        let orphans = find_orphans_under(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "empty original should be treated as crash orphan"
        );
        assert_eq!(orphans[0].backup_path, vbak);
    }

    #[test]
    fn test_check_and_recover_prompt_skips_and_warns() {
        let dir = tempfile::tempdir().unwrap();

        let backup_dir = dir.path().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join("movie.mkv.20240315120000.vbak");
        std::fs::write(&vbak, b"backup").unwrap();

        let config = RecoveryConfig {
            mode: "prompt".into(),
        };

        let store = voom_domain::test_support::InMemoryStore::default();
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        // prompt mode: nothing resolved
        assert_eq!(recovered, 0);
        // Backup still exists (not touched)
        assert!(vbak.exists());
    }
}
