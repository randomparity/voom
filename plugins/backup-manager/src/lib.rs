//! Backup Manager Plugin.
//!
//! Handles file backup before execution, disk space validation, and restore
//! capability. Creates backups of media files before any modifications are
//! applied, enabling safe rollback if execution fails.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult};
use voom_kernel::{Plugin, PluginContext};

/// A record of a backed-up file.
#[derive(Debug, Clone)]
pub struct BackupRecord {
    pub id: Uuid,
    pub original_path: PathBuf,
    pub backup_path: PathBuf,
    pub size: u64,
    pub created_at: DateTime<Utc>,
}

/// Configuration for backup operations.
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
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Backup],
            config: BackupConfig::default(),
            records: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_config(config: BackupConfig) -> Self {
        Self {
            capabilities: vec![Capability::Backup],
            config,
            records: Mutex::new(HashMap::new()),
        }
    }

    /// Create a backup of the given file before modification.
    /// Returns the BackupRecord on success.
    pub fn backup_file(&self, path: &Path) -> Result<BackupRecord> {
        // Validate the source file exists
        let metadata = fs::metadata(path).map_err(|e| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!("cannot backup {}: {}", path.display(), e),
        })?;

        // Validate disk space
        self.validate_disk_space(path)?;

        // Compute backup path and create directory
        let backup_path = self.backup_path_for(path);
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Copy file to backup location
        fs::copy(path, &backup_path).map_err(|e| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!(
                "failed to copy {} to {}: {}",
                path.display(),
                backup_path.display(),
                e
            ),
        })?;

        let record = BackupRecord {
            id: Uuid::new_v4(),
            original_path: path.to_path_buf(),
            backup_path,
            size: metadata.len(),
            created_at: Utc::now(),
        };

        let mut records = self.records.lock().unwrap();
        records.insert(path.to_path_buf(), record.clone());

        tracing::info!(
            path = %path.display(),
            backup = %record.backup_path.display(),
            size = record.size,
            "File backed up"
        );

        Ok(record)
    }

    /// Restore a file from its backup.
    pub fn restore_file(&self, path: &Path) -> Result<()> {
        let record = {
            let records = self.records.lock().unwrap();
            records.get(path).cloned()
        };

        let record = record.ok_or_else(|| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!("no backup found for {}", path.display()),
        })?;

        fs::copy(&record.backup_path, path).map_err(|e| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!(
                "failed to restore {} from {}: {}",
                path.display(),
                record.backup_path.display(),
                e
            ),
        })?;

        let mut records = self.records.lock().unwrap();
        records.remove(path);

        tracing::info!(
            path = %path.display(),
            backup = %record.backup_path.display(),
            "File restored from backup"
        );

        Ok(())
    }

    /// Remove the backup for a file (after successful execution).
    pub fn remove_backup(&self, path: &Path) -> Result<()> {
        let record = {
            let records = self.records.lock().unwrap();
            records.get(path).cloned()
        };

        let record = record.ok_or_else(|| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!("no backup found for {}", path.display()),
        })?;

        // Delete the backup file
        fs::remove_file(&record.backup_path).map_err(|e| VoomError::Plugin {
            plugin: "backup-manager".into(),
            message: format!(
                "failed to remove backup {}: {}",
                record.backup_path.display(),
                e
            ),
        })?;

        // Try to clean up the backup directory if empty
        if let Some(parent) = record.backup_path.parent() {
            if let Err(e) = fs::remove_dir(parent) {
                tracing::debug!(path = %parent.display(), error = %e, "could not remove backup parent directory");
            }
        }

        let mut records = self.records.lock().unwrap();
        records.remove(path);

        tracing::info!(
            path = %path.display(),
            "Backup removed"
        );

        Ok(())
    }

    /// Check if sufficient disk space is available for backing up the given file.
    pub fn validate_disk_space(&self, path: &Path) -> Result<()> {
        let file_size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let backup_path = self.backup_path_for(path);
        let check_path = backup_path
            .parent()
            .unwrap_or(path.parent().unwrap_or(Path::new("/")));

        let available = Self::available_space(check_path)?;
        let required = file_size + self.config.min_free_space;

        if available < required {
            return Err(VoomError::Plugin {
                plugin: "backup-manager".into(),
                message: format!(
                    "insufficient disk space for backup of {}: need {} bytes (file {} + reserve {}), have {} available",
                    path.display(),
                    required,
                    file_size,
                    self.config.min_free_space,
                    available,
                ),
            });
        }

        Ok(())
    }

    /// Get the backup path for a given original file.
    pub fn backup_path_for(&self, path: &Path) -> PathBuf {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().replace(['/', '\\', '\0'], "_"))
            .unwrap_or_else(|| "unknown".into());

        if self.config.use_global_dir {
            if let Some(ref dir) = self.config.backup_dir {
                let id = Uuid::new_v4();
                return dir.join(format!("{id}_{file_name}"));
            }
        }

        // Default: sibling .voom-backup directory
        let parent = path.parent().unwrap_or(Path::new("."));
        let timestamp = Utc::now().format("%Y%m%d%H%M%S");
        parent
            .join(".voom-backup")
            .join(format!("{file_name}.{timestamp}.bak"))
    }

    /// Check if a backup exists for the given file.
    pub fn has_backup(&self, path: &Path) -> bool {
        let records = self.records.lock().unwrap();
        records.contains_key(path)
    }

    /// Get all active backup records.
    pub fn active_backups(&self) -> Vec<BackupRecord> {
        let records = self.records.lock().unwrap();
        records.values().cloned().collect()
    }

    /// Clean up all backups (e.g., on successful completion).
    ///
    /// Returns the number of backups removed.
    pub fn cleanup_all(&self) -> Result<u64> {
        let records: Vec<BackupRecord> = {
            let records = self.records.lock().unwrap();
            records.values().cloned().collect()
        };

        let mut removed = 0u64;
        for record in &records {
            if record.backup_path.exists() {
                fs::remove_file(&record.backup_path).map_err(|e| VoomError::Plugin {
                    plugin: "backup-manager".into(),
                    message: format!(
                        "failed to remove backup {}: {}",
                        record.backup_path.display(),
                        e
                    ),
                })?;
            }
            // Try to clean up parent directory if empty
            if let Some(parent) = record.backup_path.parent() {
                if let Err(e) = fs::remove_dir(parent) {
                    tracing::debug!(path = %parent.display(), error = %e, "could not remove backup parent directory");
                }
            }
            removed += 1;
        }

        let mut records_map = self.records.lock().unwrap();
        records_map.clear();

        tracing::info!(count = removed, "All backups cleaned up");
        Ok(removed)
    }

    /// Get available disk space for a path.
    /// Walks up to the nearest existing ancestor if the path doesn't exist.
    #[cfg(unix)]
    fn available_space(path: &Path) -> Result<u64> {
        // Find the nearest existing ancestor directory
        let mut check = path.to_path_buf();
        while !check.exists() {
            match check.parent() {
                Some(p) => check = p.to_path_buf(),
                None => break,
            }
        }

        // Use libc::statvfs directly to avoid depending on df output format
        use std::ffi::CString;
        let c_path =
            CString::new(check.to_string_lossy().as_bytes()).map_err(|e| VoomError::Plugin {
                plugin: "backup-manager".into(),
                message: format!("invalid path for statvfs: {e}"),
            })?;

        unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(c_path.as_ptr(), &mut stat) == 0 {
                // Available space for unprivileged users
                Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
            } else {
                Err(VoomError::Io(std::io::Error::last_os_error()))
            }
        }
    }

    #[cfg(not(unix))]
    fn available_space(_path: &Path) -> Result<u64> {
        // On non-Unix platforms, return a large value to avoid blocking.
        Ok(u64::MAX)
    }
}

impl Default for BackupManagerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for BackupManagerPlugin {
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
            "plan.executing" | "plan.completed" | "plan.failed"
        )
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::PlanExecuting(evt) => {
                tracing::info!(
                    path = %evt.path.display(),
                    phase = %evt.phase_name,
                    "Backing up file before plan execution"
                );

                // Backup the file before execution
                self.backup_file(&evt.path)?;

                Ok(Some(EventResult {
                    plugin_name: "backup-manager".into(),
                    produced_events: vec![],
                    data: Some(serde_json::json!({
                        "backed_up": true,
                        "path": evt.path,
                        "phase": evt.phase_name,
                    })),
                    claimed: false,
                }))
            }
            Event::PlanCompleted(evt) => {
                if self.has_backup(&evt.path) {
                    tracing::info!(
                        path = %evt.path.display(),
                        phase = %evt.phase_name,
                        "Plan completed successfully, removing backup"
                    );
                    self.remove_backup(&evt.path)?;
                    Ok(Some(EventResult {
                        plugin_name: "backup-manager".into(),
                        produced_events: vec![],
                        data: Some(serde_json::json!({
                            "backup_removed": true,
                            "path": evt.path,
                            "phase": evt.phase_name,
                        })),
                        claimed: false,
                    }))
                } else {
                    Ok(None)
                }
            }
            Event::PlanFailed(evt) => {
                if self.has_backup(&evt.path) {
                    tracing::warn!(
                        path = %evt.path.display(),
                        phase = %evt.phase_name,
                        error = %evt.error,
                        "Plan failed, restoring file from backup"
                    );
                    self.restore_file(&evt.path)?;
                    Ok(Some(EventResult {
                        plugin_name: "backup-manager".into(),
                        produced_events: vec![],
                        data: Some(serde_json::json!({
                            "restored": true,
                            "path": evt.path,
                            "phase": evt.phase_name,
                            "error": evt.error,
                        })),
                        claimed: false,
                    }))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        tracing::info!("Backup manager plugin initialized");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(plugin.handles("plan.executing"));
        assert!(plugin.handles("plan.completed"));
        assert!(plugin.handles("plan.failed"));
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("plan.created"));
    }

    #[test]
    fn test_backup_path_sibling() {
        let plugin = BackupManagerPlugin::new();
        let path = Path::new("/media/movies/Movie.mkv");
        let backup = plugin.backup_path_for(path);

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
        let plugin = BackupManagerPlugin::with_config(config);
        let path = Path::new("/media/movies/Movie.mkv");
        let backup = plugin.backup_path_for(path);

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
        let plugin = BackupManagerPlugin::with_config(config);

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
        assert!(!plugin.has_backup(&file_path));
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
        let plugin = BackupManagerPlugin::with_config(config);

        let record = plugin.backup_file(&file_path).unwrap();
        let backup_path = record.backup_path.clone();
        assert!(backup_path.exists());

        // Remove backup
        plugin.remove_backup(&file_path).unwrap();
        assert!(!backup_path.exists());
        assert!(!plugin.has_backup(&file_path));
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
        let plugin = BackupManagerPlugin::with_config(config);

        assert!(!plugin.has_backup(&file_path));
        plugin.backup_file(&file_path).unwrap();
        assert!(plugin.has_backup(&file_path));
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
        let plugin = BackupManagerPlugin::with_config(config);

        plugin.backup_file(&file1).unwrap();
        plugin.backup_file(&file2).unwrap();

        let active = plugin.active_backups();
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
        let plugin = BackupManagerPlugin::with_config(config);

        let r1 = plugin.backup_file(&file1).unwrap();
        let r2 = plugin.backup_file(&file2).unwrap();

        assert!(r1.backup_path.exists());
        assert!(r2.backup_path.exists());

        let removed = plugin.cleanup_all().unwrap();
        assert_eq!(removed, 2);
        assert!(!r1.backup_path.exists());
        assert!(!r2.backup_path.exists());
        assert!(plugin.active_backups().is_empty());
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
        let plugin = BackupManagerPlugin::with_config(config);

        // Should not error with 0 min_free_space on a real filesystem
        assert!(plugin.validate_disk_space(&file_path).is_ok());
    }

    #[test]
    fn test_backup_nonexistent_file() {
        let plugin = BackupManagerPlugin::new();
        let result = plugin.backup_file(Path::new("/nonexistent/path/file.mkv"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot backup"));
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
        let plugin = BackupManagerPlugin::with_config(config);

        // Create a backup first
        let record = plugin.backup_file(&file_path).unwrap();
        assert!(record.backup_path.exists());
        assert!(plugin.has_backup(&file_path));

        // Simulate plan.completed event
        let event = Event::PlanCompleted(PlanCompletedEvent {
            plan_id: uuid::Uuid::new_v4(),
            path: file_path.clone(),
            phase_name: "normalize".into(),
            actions_applied: 3,
        });

        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "backup-manager");
        assert!(!plugin.has_backup(&file_path));
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
        let plugin = BackupManagerPlugin::with_config(config);

        // Create a backup
        plugin.backup_file(&file_path).unwrap();
        assert!(plugin.has_backup(&file_path));

        // Modify the original file (simulating a failed execution)
        fs::write(&file_path, b"corrupted data").unwrap();

        // Simulate plan.failed event
        let event = Event::PlanFailed(PlanFailedEvent {
            plan_id: uuid::Uuid::new_v4(),
            path: file_path.clone(),
            phase_name: "normalize".into(),
            error: "ffmpeg crashed".into(),
            error_code: None,
            plugin_name: None,
        });

        let result = plugin.on_event(&event).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "backup-manager");

        // File should be restored to original content
        assert_eq!(fs::read(&file_path).unwrap(), original_content);
        assert!(!plugin.has_backup(&file_path));
    }

    #[test]
    fn test_handles_plan_completed_and_failed() {
        let plugin = BackupManagerPlugin::new();
        assert!(plugin.handles("plan.executing"));
        assert!(plugin.handles("plan.completed"));
        assert!(plugin.handles("plan.failed"));
        assert!(!plugin.handles("file.discovered"));
        assert!(!plugin.handles("plan.created"));
    }

    #[test]
    fn test_backup_path_sanitizes_traversal() {
        let config = BackupConfig {
            backup_dir: Some(PathBuf::from("/tmp/voom-backups")),
            use_global_dir: true,
            min_free_space: 0,
        };
        let plugin = BackupManagerPlugin::with_config(config);

        // Path traversal characters should be replaced with underscores
        let path = Path::new("/media/movies/../../../etc/passwd");
        let backup = plugin.backup_path_for(path);
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
        let backup = plugin.backup_path_for(path);
        let filename = backup.file_name().unwrap().to_string_lossy();
        assert!(!filename.contains('\0'));
    }

    #[test]
    fn test_backup_path_normal_filename() {
        let plugin = BackupManagerPlugin::new();
        let path = Path::new("/media/movies/My Movie (2024).mkv");
        let backup = plugin.backup_path_for(path);
        let backup_str = backup.to_string_lossy();
        assert!(backup_str.contains("My Movie (2024).mkv"));
    }
}
