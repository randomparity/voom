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
    let all_backups = find_orphans_under(scan_dirs)?;
    if all_backups.is_empty() {
        return Ok(0);
    }

    let orphans: Vec<_> = all_backups
        .into_iter()
        .filter(|b| is_crash_orphan(&b.original_path, store))
        .collect();

    if orphans.is_empty() {
        tracing::debug!("found backup files, none are crash orphans");
        return Ok(0);
    }

    tracing::info!(
        count = orphans.len(),
        "found orphaned backup files from crashed executions"
    );

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
///
/// Returns all backup files found; callers must use `is_crash_orphan` to filter
/// to genuine orphans from crashed executions.
fn find_orphans_under(dirs: &[PathBuf]) -> Result<Vec<OrphanedBackup>> {
    let mut backups = Vec::new();

    for dir in dirs {
        collect_orphans_in(dir, &mut backups);
    }

    Ok(backups)
}

/// Check whether a backup is from an incomplete execution (true = orphan)
/// or a retained backup from a completed execution (false = not orphan).
fn is_crash_orphan(original_path: &Path, store: &dyn voom_domain::storage::StorageTrait) -> bool {
    let path_str = original_path.to_string_lossy();
    let path_prefix = format!("path={path_str} ");

    let mut filters = voom_domain::storage::EventLogFilters::default();
    filters.event_type = Some("plan.*".into());

    let events = match store.list_event_log(&filters) {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query event log for recovery");
            // Can't determine — treat conservatively as NOT an orphan.
            return false;
        }
    };

    // Find events for this specific path, in chronological order (rowid ASC).
    // Summary format: "path=/some/path phase=some_phase"
    let path_events: Vec<_> = events
        .iter()
        .filter(|e| e.summary.starts_with(&path_prefix))
        .collect();

    if path_events.is_empty() {
        // No events for this path — ambiguous. Not enough evidence to call it an orphan.
        tracing::debug!(
            path = %original_path.display(),
            "no plan events found in event log — skipping recovery"
        );
        return false;
    }

    // Events are ordered rowid ASC so the last element is the most recent.
    let last_event = path_events.last().expect("checked non-empty above");

    match last_event.event_type.as_str() {
        "plan.executing" => {
            // Last event was executing with no completion — crash orphan.
            true
        }
        "plan.completed" | "plan.failed" => {
            // Execution completed — this is a retained backup, not an orphan.
            false
        }
        _ => false,
    }
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
    use voom_domain::EventLogStorage as _;

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

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_backup(dir: &std::path::Path, filename: &str) -> (PathBuf, PathBuf) {
        let backup_dir = dir.join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak = backup_dir.join(format!("{filename}.20240315120000.vbak"));
        std::fs::write(&vbak, b"backup content").unwrap();
        let original = dir.join(filename);
        (vbak, original)
    }

    fn insert_executing_event(
        store: &voom_domain::test_support::InMemoryStore,
        original_path: &std::path::Path,
    ) {
        use voom_domain::storage::EventLogRecord;
        let record = EventLogRecord::new(
            uuid::Uuid::new_v4(),
            "plan.executing".to_string(),
            "{}".to_string(),
            format!("path={} phase=test", original_path.display()),
        );
        store.insert_event_log(&record).unwrap();
    }

    fn insert_completed_event(
        store: &voom_domain::test_support::InMemoryStore,
        original_path: &std::path::Path,
    ) {
        use voom_domain::storage::EventLogRecord;
        let record = EventLogRecord::new(
            uuid::Uuid::new_v4(),
            "plan.completed".to_string(),
            "{}".to_string(),
            format!("path={} phase=test", original_path.display()),
        );
        store.insert_event_log(&record).unwrap();
    }

    // ── is_crash_orphan ──────────────────────────────────────────────────────

    #[test]
    fn test_is_crash_orphan_executing_event_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);

        assert!(is_crash_orphan(&original, &store));
    }

    #[test]
    fn test_is_crash_orphan_completed_after_executing_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);
        insert_completed_event(&store, &original);

        assert!(!is_crash_orphan(&original, &store));
    }

    #[test]
    fn test_is_crash_orphan_no_events_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        assert!(!is_crash_orphan(&original, &store));
    }

    #[test]
    fn test_is_crash_orphan_only_completed_event_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_completed_event(&store, &original);

        assert!(!is_crash_orphan(&original, &store));
    }

    #[test]
    fn test_is_crash_orphan_ignores_events_for_different_path() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("movie.mkv");
        let other = dir.path().join("other.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        // Insert executing event for a different path
        insert_executing_event(&store, &other);

        // No events for our path — conservative: not an orphan
        assert!(!is_crash_orphan(&original, &store));
    }

    // ── check_and_recover_under modes ────────────────────────────────────────

    #[test]
    fn test_check_and_recover_always_recover_restores_file() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 1);
        // Original should now have the backup content
        let content = std::fs::read(&original).unwrap();
        assert_eq!(content, b"backup content");
        // Backup file should be gone
        assert!(!vbak.exists(), "backup should be removed after restore");
    }

    #[test]
    fn test_check_and_recover_always_discard_removes_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);

        let config = RecoveryConfig {
            mode: "always_discard".into(),
        };
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
    fn test_check_and_recover_skips_retained_backup_with_completed_event() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);
        insert_completed_event(&store, &original);

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        // Execution completed — backup is retained, not an orphan
        assert_eq!(recovered, 0);
        // Backup must be untouched
        assert!(vbak.exists(), "retained backup should not be removed");
    }

    #[test]
    fn test_check_and_recover_skips_backup_with_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, _original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        // No events inserted

        let config = RecoveryConfig {
            mode: "always_recover".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        assert_eq!(recovered, 0);
        assert!(
            vbak.exists(),
            "backup with no event log evidence should not be touched"
        );
    }

    #[test]
    fn test_check_and_recover_prompt_skips_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let (vbak, original) = make_backup(dir.path(), "movie.mkv");

        let store = voom_domain::test_support::InMemoryStore::default();
        insert_executing_event(&store, &original);

        let config = RecoveryConfig {
            mode: "prompt".into(),
        };
        let recovered =
            check_and_recover_under(&config, &[dir.path().to_path_buf()], &store).unwrap();

        // prompt mode: nothing resolved
        assert_eq!(recovered, 0);
        // Backup still exists (not touched)
        assert!(vbak.exists());
    }
}
