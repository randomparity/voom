//! Backup, restore, and cleanup operations.
//!
//! Contains the core file backup logic: creating backups via `fs::copy`,
//! restoring from backup, removing individual backups, and bulk cleanup.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;
use voom_domain::errors::Result;

use crate::destination::{
    remote_path_for, upload_with_rclone, RemoteBackupRecord, RemoteUploadReceipt,
    RemoteUploadRequest,
};
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
    backup_file_with_destinations(config, path, validate_space, upload_with_rclone)
}

/// Create a backup and upload it to configured remote destinations.
///
/// The upload runner is injected so unit tests can verify behavior without
/// invoking rclone.
pub fn backup_file_with_destinations(
    config: &BackupConfig,
    path: &Path,
    validate_space: impl FnOnce(&Path, &Path) -> Result<()>,
    mut upload_remote: impl FnMut(RemoteUploadRequest<'_>) -> Result<RemoteUploadReceipt>,
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

    let mut remote_backups = Vec::new();
    for destination in &config.destinations {
        if !destination.kind.is_rclone_backed() {
            continue;
        }
        let remote_path = remote_path_for(destination, backup_id, path)?;
        let request = RemoteUploadRequest {
            destination,
            source_path: path,
            remote_path: remote_path.clone(),
            expected_size: metadata.len(),
            rclone_path: &config.rclone_path,
            verify_after_upload: config.verify_after_upload,
        };
        match upload_remote(request) {
            Ok(receipt) => remote_backups.push(RemoteBackupRecord {
                destination_name: destination.name.clone(),
                remote_path,
                verified: receipt.verified,
            }),
            Err(e) if config.block_on_remote_failure => return Err(e),
            Err(e) => tracing::warn!(
                destination = %destination.name,
                error = %e,
                "remote backup upload failed; continuing because block_on_remote_failure=false"
            ),
        }
    }

    let record = BackupRecord {
        id: backup_id,
        original_path: path.to_path_buf(),
        backup_path,
        size: metadata.len(),
        created_at: Utc::now(),
        remote_backups,
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

    use crate::destination::{BackupDestinationConfig, DestinationKind};

    use super::*;

    fn global_config(dir: &Path) -> BackupConfig {
        BackupConfig {
            backup_dir: Some(dir.to_path_buf()),
            use_global_dir: true,
            min_free_space: 0,
            verify_after_upload: true,
            block_on_remote_failure: true,
            rclone_path: "rclone".to_string(),
            destinations: Vec::new(),
        }
    }

    fn remote_test_config(dir: &Path) -> BackupConfig {
        BackupConfig {
            destinations: vec![
                BackupDestinationConfig::rclone("offsite", "b2:voom"),
                BackupDestinationConfig {
                    name: "archive-s3".to_string(),
                    kind: DestinationKind::S3,
                    remote: Some("aws:voom".to_string()),
                    bandwidth_limit: Some("10M".to_string()),
                    minimum_storage_days: None,
                },
            ],
            ..global_config(dir)
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
    fn backup_file_uploads_to_all_remote_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("movie.mkv");
        fs::write(&source, b"movie").unwrap();
        let config = remote_test_config(dir.path());
        let uploads = RefCell::new(Vec::new());

        let record = backup_file_with_destinations(
            &config,
            &source,
            |_backup, _source| Ok(()),
            |request| {
                uploads.borrow_mut().push((
                    request.destination.name.clone(),
                    request.remote_path.clone(),
                    request.source_path.to_path_buf(),
                ));
                Ok(RemoteUploadReceipt { verified: true })
            },
        )
        .unwrap();

        assert_eq!(record.remote_backups.len(), 2);
        assert_eq!(uploads.borrow().len(), 2);
        assert!(record
            .remote_backups
            .iter()
            .any(|backup| backup.destination_name == "offsite"));
        assert!(record
            .remote_backups
            .iter()
            .any(|backup| backup.destination_name == "archive-s3"));
    }

    #[test]
    fn remote_upload_failure_blocks_backup() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("movie.mkv");
        fs::write(&source, b"movie").unwrap();
        let config = remote_test_config(dir.path());

        let err = backup_file_with_destinations(
            &config,
            &source,
            |_backup, _source| Ok(()),
            |_request| Err(plugin_err("offsite upload failed")),
        )
        .unwrap_err();

        assert!(err.to_string().contains("offsite upload failed"));
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
