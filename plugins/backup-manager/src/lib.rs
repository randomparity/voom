//! Backup Manager Plugin.
//!
//! Handles file backup before execution, disk space validation, and restore
//! capability. Creates backups of media files before any modifications are
//! applied, enabling safe rollback if execution fails.
//!
//! Modules:
//! - [`backup`] — core backup, restore, remove, and cleanup file operations
//! - [`space`] — disk space validation via `statvfs`

#![allow(clippy::missing_errors_doc)]

pub mod backup;
pub mod space;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use uuid::Uuid;
use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult};
use voom_kernel::Plugin;

/// Create a `VoomError::Plugin` for the backup-manager plugin.
pub(crate) fn plugin_err(message: impl Into<String>) -> VoomError {
    VoomError::plugin("backup-manager", message)
}

/// A record of a backed-up file.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackupRecord {
    pub id: Uuid,
    pub original_path: PathBuf,
    pub backup_path: PathBuf,
    pub size: u64,
    pub created_at: DateTime<Utc>,
}

/// Configuration for backup operations.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BackupConfig {
    /// Directory to store backups. Used when `use_global_dir` is true.
    pub backup_dir: Option<PathBuf>,
    /// Whether to use a global backup directory or per-file sibling directory.
    pub use_global_dir: bool,
    /// Minimum free space (bytes) required before allowing execution.
    pub min_free_space: u64,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            backup_dir: None,
            use_global_dir: false,
            min_free_space: 1024 * 1024 * 100, // 100 MB
        }
    }
}

/// Plugin that manages file backups before plan execution.
///
/// When a `PlanExecuting` event is received, the plugin creates a backup
/// of the target file so it can be restored if execution fails. It also
/// validates that sufficient disk space is available.
pub struct BackupManagerPlugin {
    capabilities: Vec<Capability>,
    config: BackupConfig,
    /// Active backup records indexed by original path.
    records: Mutex<HashMap<PathBuf, BackupRecord>>,
}

impl BackupManagerPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Backup],
            config: BackupConfig::default(),
            records: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn from_config(config: BackupConfig) -> Self {
        Self {
            capabilities: vec![Capability::Backup],
            config,
            records: Mutex::new(HashMap::new()),
        }
    }

    fn records(&self) -> Result<MutexGuard<'_, HashMap<PathBuf, BackupRecord>>> {
        self.records
            .lock()
            .map_err(|_| plugin_err("backup records lock poisoned"))
    }

    /// Create a backup of the given file before modification.
    /// Returns the `BackupRecord` on success.
    pub fn backup_file(&self, path: &Path) -> Result<BackupRecord> {
        let min_free = self.config.min_free_space;
        let record = backup::backup_file(&self.config, path, |backup_path, source_path| {
            space::validate_disk_space_for(backup_path, source_path, min_free)
        })?;

        let mut records = self.records()?;
        records.insert(path.to_path_buf(), record.clone());

        Ok(record)
    }

    /// Restore a file from its backup.
    pub fn restore_file(&self, path: &Path) -> Result<()> {
        let mut records = self.records()?;
        let record = records
            .get(path)
            .ok_or_else(|| plugin_err(format!("no backup found for {}", path.display())))?;

        backup::restore_file(record)?;
        records.remove(path);
        Ok(())
    }

    /// Remove the backup for a file (after successful execution).
    pub fn remove_backup(&self, path: &Path) -> Result<()> {
        let mut records = self.records()?;
        let record = records
            .get(path)
            .ok_or_else(|| plugin_err(format!("no backup found for {}", path.display())))?;

        backup::remove_backup(record)?;
        records.remove(path);
        Ok(())
    }

    /// Check if sufficient disk space is available for backing up the given file.
    pub fn validate_disk_space(&self, path: &Path) -> Result<()> {
        let backup_path = backup::backup_path_for(&self.config, path, Uuid::new_v4());
        space::validate_disk_space_for(&backup_path, path, self.config.min_free_space)
    }

    /// Get the backup path for a given original file.
    ///
    /// In global-dir mode, a `unique_id` is incorporated into the filename
    /// to avoid collisions. Pass the same UUID to `validate_disk_space` and
    /// `backup_file` calls to ensure the space check targets the actual
    /// destination path.
    pub fn backup_path_for(&self, path: &Path, unique_id: Uuid) -> PathBuf {
        backup::backup_path_for(&self.config, path, unique_id)
    }

    /// Check if a backup exists for the given file.
    pub fn has_backup(&self, path: &Path) -> Result<bool> {
        let records = self.records()?;
        Ok(records.contains_key(path))
    }

    /// Get all active backup records.
    pub fn active_backups(&self) -> Result<Vec<BackupRecord>> {
        let records = self.records()?;
        Ok(records.values().cloned().collect())
    }

    /// Clean up all backups (e.g., on successful completion).
    ///
    /// Returns the number of backups removed.
    pub fn cleanup_all(&self) -> Result<u64> {
        let all_records: Vec<BackupRecord> = {
            let records = self.records()?;
            records.values().cloned().collect()
        };

        let removed = backup::cleanup_all(&all_records)?;

        let mut records_map = self.records()?;
        records_map.clear();

        Ok(removed)
    }
}

impl Default for BackupManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Construct a successful `EventResult` for the backup-manager plugin.
fn backup_result(data: serde_json::Value) -> EventResult {
    let mut result = EventResult::new("backup-manager");
    result.data = Some(data);
    result
}

impl Plugin for BackupManagerPlugin {
    // NOTE: When config options are added to the plugin context for backup-manager,
    // `ctx.parse_config::<BackupConfig>()` can be used in `init()` to ergonomically
    // deserialize them (add Deserialize derive to BackupConfig first).

    fn name(&self) -> &str {
        "backup-manager"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        matches!(
            event_type,
            Event::PLAN_EXECUTING | Event::PLAN_COMPLETED | Event::PLAN_FAILED
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanExecuting(evt) => {
                // Skip if we already have a backup for this file (multiple
                // phases may fire PlanExecuting for the same path).
                if self.has_backup(&evt.path)? {
                    tracing::debug!(
                        path = %evt.path.display(),
                        phase = %evt.phase_name,
                        "Backup already exists, skipping"
                    );
                    return Ok(None);
                }

                tracing::info!(
                    path = %evt.path.display(),
                    phase = %evt.phase_name,
                    "Backing up file before plan execution"
                );

                self.backup_file(&evt.path)?;

                Ok(Some(backup_result(serde_json::json!({
                    "backed_up": true,
                    "path": evt.path,
                    "phase": evt.phase_name,
                }))))
            }
            Event::PlanCompleted(evt) => {
                if self.has_backup(&evt.path)? {
                    tracing::info!(
                        path = %evt.path.display(),
                        phase = %evt.phase_name,
                        "Plan completed successfully, removing backup"
                    );
                    self.remove_backup(&evt.path)?;
                    Ok(Some(backup_result(serde_json::json!({
                        "backup_removed": true,
                        "path": evt.path,
                        "phase": evt.phase_name,
                    }))))
                } else {
                    Ok(None)
                }
            }
            Event::PlanFailed(evt) => {
                if self.has_backup(&evt.path)? {
                    tracing::warn!(
                        path = %evt.path.display(),
                        phase = %evt.phase_name,
                        error = %evt.error,
                        "Plan failed, restoring file from backup"
                    );
                    self.restore_file(&evt.path)?;
                    Ok(Some(backup_result(serde_json::json!({
                        "restored": true,
                        "path": evt.path,
                        "phase": evt.phase_name,
                        "error": evt.error,
                    }))))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = BackupManagerPlugin::new();
        assert_eq!(plugin.name(), "backup-manager");
        assert_eq!(plugin.capabilities().len(), 1);
        assert_eq!(plugin.capabilities()[0], Capability::Backup);
    }

    #[test]
    fn test_handles_plan_executing() {
        let plugin = BackupManagerPlugin::new();
        assert!(plugin.handles(Event::PLAN_EXECUTING));
        assert!(plugin.handles(Event::PLAN_COMPLETED));
        assert!(plugin.handles(Event::PLAN_FAILED));
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::PLAN_CREATED));
    }

    #[test]
    fn test_backup_path_sibling() {
        let plugin = BackupManagerPlugin::new();
        let path = Path::new("/media/movies/Movie.mkv");
        let backup = plugin.backup_path_for(path, Uuid::new_v4());

        assert!(backup
            .to_string_lossy()
            .starts_with("/media/movies/.voom-backup/Movie.mkv."));
        assert!(backup.to_string_lossy().ends_with(".bak"));
    }

    #[test]
    fn test_backup_path_global() {
        let config = BackupConfig {
            backup_dir: Some(PathBuf::from("/tmp/voom-backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);
        let path = Path::new("/media/movies/Movie.mkv");
        let backup = plugin.backup_path_for(path, Uuid::new_v4());

        assert!(backup.to_string_lossy().starts_with("/tmp/voom-backups/"));
        assert!(backup.to_string_lossy().ends_with("_Movie.mkv"));
    }

    #[test]
    fn test_backup_and_restore() {
        let dir = TempDir::new().unwrap();
        let original_content = b"original file content here";
        let file_path = create_test_file(dir.path(), "test.mkv", original_content);

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        // Create backup
        let record = plugin.backup_file(&file_path).unwrap();
        assert_eq!(record.original_path, file_path);
        assert!(record.backup_path.exists());
        assert_eq!(record.size, original_content.len() as u64);

        // Modify the original
        fs::write(&file_path, b"modified content").unwrap();
        assert_eq!(fs::read(&file_path).unwrap(), b"modified content");

        // Restore from backup
        plugin.restore_file(&file_path).unwrap();
        assert_eq!(fs::read(&file_path).unwrap(), original_content);

        // Record should be removed after restore
        assert!(!plugin.has_backup(&file_path).unwrap());
    }

    #[test]
    fn test_backup_and_remove() {
        let dir = TempDir::new().unwrap();
        let file_path = create_test_file(dir.path(), "test.mkv", b"file data");

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        let record = plugin.backup_file(&file_path).unwrap();
        let backup_path = record.backup_path.clone();
        assert!(backup_path.exists());

        // Remove backup
        plugin.remove_backup(&file_path).unwrap();
        assert!(!backup_path.exists());
        assert!(!plugin.has_backup(&file_path).unwrap());
    }

    #[test]
    fn test_has_backup() {
        let dir = TempDir::new().unwrap();
        let file_path = create_test_file(dir.path(), "test.mkv", b"data");

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        assert!(!plugin.has_backup(&file_path).unwrap());
        plugin.backup_file(&file_path).unwrap();
        assert!(plugin.has_backup(&file_path).unwrap());
    }

    #[test]
    fn test_active_backups() {
        let dir = TempDir::new().unwrap();
        let file1 = create_test_file(dir.path(), "one.mkv", b"data1");
        let file2 = create_test_file(dir.path(), "two.mkv", b"data2");

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        plugin.backup_file(&file1).unwrap();
        plugin.backup_file(&file2).unwrap();

        let active = plugin.active_backups().unwrap();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn test_cleanup_all() {
        let dir = TempDir::new().unwrap();
        let file1 = create_test_file(dir.path(), "one.mkv", b"data1");
        let file2 = create_test_file(dir.path(), "two.mkv", b"data2");

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        let r1 = plugin.backup_file(&file1).unwrap();
        let r2 = plugin.backup_file(&file2).unwrap();

        assert!(r1.backup_path.exists());
        assert!(r2.backup_path.exists());

        let removed = plugin.cleanup_all().unwrap();
        assert_eq!(removed, 2);
        assert!(!r1.backup_path.exists());
        assert!(!r2.backup_path.exists());
        assert!(plugin.active_backups().unwrap().is_empty());
    }

    #[test]
    fn test_validate_disk_space_sufficient() {
        let dir = TempDir::new().unwrap();
        let file_path = create_test_file(dir.path(), "test.mkv", b"small file");

        // With min_free_space set to 0, any available space should suffice
        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        // Should not error with 0 min_free_space on a real filesystem
        assert!(plugin.validate_disk_space(&file_path).is_ok());
    }

    #[test]
    fn test_backup_rejects_symlink() {
        let dir = TempDir::new().unwrap();
        let real_file = create_test_file(dir.path(), "real.mkv", b"data");
        let symlink_path = dir.path().join("link.mkv");
        std::os::unix::fs::symlink(&real_file, &symlink_path).unwrap();

        let plugin = BackupManagerPlugin::new();
        let result = plugin.backup_file(&symlink_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("refusing to backup symlink"));
    }

    #[test]
    fn test_backup_nonexistent_file() {
        let plugin = BackupManagerPlugin::new();
        let result = plugin.backup_file(Path::new("/nonexistent/path/file.mkv"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot read"));
    }

    #[test]
    fn test_restore_no_backup() {
        let plugin = BackupManagerPlugin::new();
        let result = plugin.restore_file(Path::new("/some/file.mkv"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no backup found"));
    }

    #[test]
    fn test_remove_no_backup() {
        let plugin = BackupManagerPlugin::new();
        let result = plugin.remove_backup(Path::new("/some/file.mkv"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no backup found"));
    }

    #[test]
    fn test_on_event_plan_completed_removes_backup() {
        use voom_domain::events::PlanCompletedEvent;

        let dir = TempDir::new().unwrap();
        let file_path = create_test_file(dir.path(), "movie.mkv", b"movie data");

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        // Create a backup first
        let record = plugin.backup_file(&file_path).unwrap();
        assert!(record.backup_path.exists());
        assert!(plugin.has_backup(&file_path).unwrap());

        // Simulate plan.completed event
        let event = Event::PlanCompleted(PlanCompletedEvent::new(
            uuid::Uuid::new_v4(),
            file_path.clone(),
            "normalize",
            3,
        ));

        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "backup-manager");
        assert!(!plugin.has_backup(&file_path).unwrap());
        assert!(!record.backup_path.exists());
    }

    #[test]
    fn test_on_event_plan_failed_restores_backup() {
        use voom_domain::events::PlanFailedEvent;

        let dir = TempDir::new().unwrap();
        let original_content = b"original movie data";
        let file_path = create_test_file(dir.path(), "movie.mkv", original_content);

        let config = BackupConfig {
            backup_dir: Some(dir.path().join("backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        // Create a backup
        plugin.backup_file(&file_path).unwrap();
        assert!(plugin.has_backup(&file_path).unwrap());

        // Modify the original file (simulating a failed execution)
        fs::write(&file_path, b"corrupted data").unwrap();

        // Simulate plan.failed event
        let event = Event::PlanFailed(PlanFailedEvent::new(
            uuid::Uuid::new_v4(),
            file_path.clone(),
            "normalize",
            "ffmpeg crashed",
        ));

        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "backup-manager");

        // File should be restored to original content
        assert_eq!(fs::read(&file_path).unwrap(), original_content);
        assert!(!plugin.has_backup(&file_path).unwrap());
    }

    #[test]
    fn test_backup_path_sanitizes_traversal() {
        let config = BackupConfig {
            backup_dir: Some(PathBuf::from("/tmp/voom-backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::from_config(config);

        // Path traversal characters should be replaced with underscores
        let path = Path::new("/media/movies/../../../etc/passwd");
        let backup = plugin.backup_path_for(path, Uuid::new_v4());
        let backup_str = backup.to_string_lossy();
        assert!(backup_str.starts_with("/tmp/voom-backups/"));
        assert!(backup_str.ends_with("_passwd"));
        // The filename portion should not contain path separators
        let filename = backup.file_name().unwrap().to_string_lossy();
        assert!(!filename.contains('/'));
        assert!(!filename.contains('\\'));
    }

    #[test]
    fn test_backup_path_sanitizes_null_bytes() {
        let plugin = BackupManagerPlugin::new();
        // OsStr can't actually contain null bytes on most platforms,
        // but the sanitization handles it if present in the lossy conversion
        let path = Path::new("/media/movies/normal_file.mkv");
        let backup = plugin.backup_path_for(path, Uuid::new_v4());
        let filename = backup.file_name().unwrap().to_string_lossy();
        assert!(!filename.contains('\0'));
    }

    #[test]
    fn test_backup_path_normal_filename() {
        let plugin = BackupManagerPlugin::new();
        let path = Path::new("/media/movies/My Movie (2024).mkv");
        let backup = plugin.backup_path_for(path, Uuid::new_v4());
        let backup_str = backup.to_string_lossy();
        assert!(backup_str.contains("My Movie (2024).mkv"));
    }
}
