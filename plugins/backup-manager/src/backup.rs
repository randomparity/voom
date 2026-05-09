//! Backup, restore, and cleanup operations.
//!
//! Contains the core file backup logic: creating backups via `fs::copy`,
//! restoring from backup, removing individual backups, and bulk cleanup.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;
use voom_domain::errors::Result;

use crate::{plugin_err, BackupConfig, BackupRecord};

/// Create a backup of the given file.
///
/// Returns the `BackupRecord` on success. The backup is written to the path
/// determined by [`backup_path_for`].
pub fn backup_file(
    config: &BackupConfig,
    path: &Path,
    validate_space: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<BackupRecord> {
    // Reject symlinks to prevent following links to unintended targets
    let symlink_meta = fs::symlink_metadata(path)
        .map_err(|e| plugin_err(format!("cannot read {}: {e}", path.display())))?;
    if symlink_meta.is_symlink() {
        return Err(plugin_err(format!(
            "refusing to backup symlink: {}",
            path.display()
        )));
    }

    let metadata = fs::metadata(path)
        .map_err(|e| plugin_err(format!("cannot backup {}: {e}", path.display())))?;

    // Generate UUID once so disk-space check and backup target are consistent
    let backup_id = Uuid::new_v4();

    // Validate disk space against the actual backup destination
    let backup_path = backup_path_for(config, path, backup_id);
    validate_space(&backup_path, path)?;
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::copy(path, &backup_path).map_err(|e| {
        plugin_err(format!(
            "failed to copy {} to {}: {e}",
            path.display(),
            backup_path.display(),
        ))
    })?;

    let record = BackupRecord {
        id: backup_id,
        original_path: path.to_path_buf(),
        backup_path,
        size: metadata.len(),
        created_at: Utc::now(),
        retained: false,
    };

    tracing::info!(
        path = %path.display(),
        backup = %record.backup_path.display(),
        size = record.size,
        "File backed up"
    );

    Ok(record)
}

/// Restore a file from its backup record, copying the backup back to the
/// original path.
pub fn restore_file(record: &BackupRecord) -> Result<()> {
    fs::copy(&record.backup_path, &record.original_path).map_err(|e| {
        plugin_err(format!(
            "failed to restore {} from {}: {e}",
            record.original_path.display(),
            record.backup_path.display(),
        ))
    })?;

    tracing::info!(
        path = %record.original_path.display(),
        backup = %record.backup_path.display(),
        "File restored from backup"
    );

    Ok(())
}

/// Remove a single backup file from disk and clean up the parent directory
/// if empty.
pub fn remove_backup(record: &BackupRecord) -> Result<()> {
    fs::remove_file(&record.backup_path).map_err(|e| {
        plugin_err(format!(
            "failed to remove backup {}: {e}",
            record.backup_path.display(),
        ))
    })?;

    try_cleanup_parent_dir(&record.backup_path);

    tracing::info!(
        path = %record.original_path.display(),
        "Backup removed"
    );

    Ok(())
}

/// Remove all backup files in the given list from disk.
///
/// Returns the number of backups successfully removed.
pub fn cleanup_all(records: &[BackupRecord]) -> Result<u64> {
    let mut removed = 0u64;
    for record in records {
        if record.backup_path.exists() {
            fs::remove_file(&record.backup_path).map_err(|e| {
                plugin_err(format!(
                    "failed to remove backup {}: {e}",
                    record.backup_path.display(),
                ))
            })?;
            removed += 1;
        }
        try_cleanup_parent_dir(&record.backup_path);
    }

    tracing::info!(count = removed, "All backups cleaned up");
    Ok(removed)
}

/// Restore a file from its backup path to the derived original path.
///
/// Unlike [`restore_file`], this does not require a `BackupRecord`. The
/// original path is derived by stripping the `.YYYYMMDDHHMMSS.vbak` suffix
/// from the backup filename. This is intended for CLI use where backup
/// records are not available (the plugin's state is in-memory only).
pub fn restore_from_paths(backup_path: &Path, original_path: &Path) -> Result<()> {
    fs::copy(backup_path, original_path).map_err(|e| {
        plugin_err(format!(
            "failed to restore {} from {}: {e}",
            original_path.display(),
            backup_path.display(),
        ))
    })?;
    tracing::info!(
        original = %original_path.display(),
        backup = %backup_path.display(),
        "File restored from backup"
    );
    Ok(())
}

/// Remove a `.vbak` file from disk and clean up its parent directory if empty.
///
/// This is a standalone helper for CLI use where no `BackupRecord` is
/// available.
pub fn remove_vbak_file(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .map_err(|e| plugin_err(format!("failed to remove backup {}: {e}", path.display(),)))?;
    try_cleanup_parent_dir(path);
    Ok(())
}

/// Try to remove the parent directory of a backup file if it is empty.
///
/// This is best-effort: failures are logged at debug level and ignored.
fn try_cleanup_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::remove_dir(parent) {
            tracing::debug!(
                path = %parent.display(),
                error = %e,
                "could not remove backup parent directory"
            );
        }
    }
}

/// Compute the backup path for a given original file.
///
/// In global-dir mode, a `unique_id` is incorporated into the filename
/// to avoid collisions. In sibling mode, a timestamp-based name is used
/// in a `.voom-backup` directory next to the original.
#[must_use]
pub fn backup_path_for(config: &BackupConfig, path: &Path, unique_id: Uuid) -> PathBuf {
    let file_name = path.file_name().map_or_else(
        || "unknown".into(),
        |n| n.to_string_lossy().replace(['/', '\\', '\0'], "_"),
    );

    if config.use_global_dir {
        if let Some(ref dir) = config.backup_dir {
            return dir.join(format!("{unique_id}_{file_name}"));
        }
    }

    // Default: sibling .voom-backup directory
    let parent = path.parent().unwrap_or(Path::new("."));
    let timestamp = Utc::now().format("%Y%m%d%H%M%S");
    parent
        .join(".voom-backup")
        .join(format!("{file_name}.{timestamp}.vbak"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn global_config(dir: &Path) -> BackupConfig {
        BackupConfig {
            backup_dir: Some(dir.to_path_buf()),
            use_global_dir: true,
            min_free_space: 0,
        }
    }

    #[test]
    fn backup_file_validates_actual_destination() {
        let source_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("movie.mkv");
        fs::write(&source, b"movie").unwrap();

        let validated = RefCell::new(None);
        let record = backup_file(
            &global_config(backup_dir.path()),
            &source,
            |backup, original| {
                assert_eq!(original, source.as_path());
                assert!(backup.starts_with(backup_dir.path()));
                *validated.borrow_mut() = Some(backup.to_path_buf());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(validated.into_inner(), Some(record.backup_path.clone()));
        assert_eq!(fs::read(&record.backup_path).unwrap(), b"movie");
    }

    #[test]
    fn backup_file_validation_error_prevents_copy() {
        let source_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("movie.mkv");
        fs::write(&source, b"movie").unwrap();

        let err = backup_file(
            &global_config(backup_dir.path()),
            &source,
            |_backup, _original| Err(plugin_err("no space")),
        )
        .unwrap_err();

        assert!(err.to_string().contains("no space"));
        assert!(fs::read_dir(backup_dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn restore_from_paths_writes_requested_destination() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup.vbak");
        let destination = dir.path().join("restored").join("movie.mkv");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&backup, b"backup bytes").unwrap();

        restore_from_paths(&backup, &destination).unwrap();

        assert_eq!(fs::read(destination).unwrap(), b"backup bytes");
    }

    #[test]
    fn remove_vbak_file_removes_file_and_empty_parent() {
        let dir = tempfile::tempdir().unwrap();
        let backup_parent = dir.path().join(".voom-backup");
        fs::create_dir_all(&backup_parent).unwrap();
        let backup = backup_parent.join("movie.mkv.20260101000000.vbak");
        fs::write(&backup, b"backup").unwrap();

        remove_vbak_file(&backup).unwrap();

        assert!(!backup.exists());
        assert!(!backup_parent.exists());
    }
}
