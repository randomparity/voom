//! Disk space validation.
//!
//! Re-exports the shared `available_space` from `voom-domain` and provides
//! a backup-specific `validate_disk_space_for` wrapper.

use std::fs;
use std::path::Path;

use voom_domain::errors::Result;

use crate::plugin_err;

// Re-export so existing callers within backup-manager don't break.
pub use voom_domain::utils::disk::available_space;

/// Check that sufficient disk space is available for backing up `source_path`
/// to `backup_path`.
///
/// The check requires `file_size + min_free_space` bytes available on the
/// filesystem that contains `backup_path`.
pub fn validate_disk_space_for(
    backup_path: &Path,
    source_path: &Path,
    min_free_space: u64,
) -> Result<()> {
    let file_size = fs::metadata(source_path)
        .map_err(|e| {
            plugin_err(format!(
                "failed to read metadata for {}: {e}",
                source_path.display()
            ))
        })?
        .len();

    let check_path = backup_path
        .parent()
        .unwrap_or(source_path.parent().unwrap_or(Path::new("/")));

    let available = available_space(check_path)?;
    let required = file_size + min_free_space;

    if available < required {
        return Err(plugin_err(format!(
            "insufficient disk space for backup of {}: need {} bytes (file {} + reserve {}), have {} available",
            source_path.display(),
            required,
            file_size,
            min_free_space,
            available,
        )));
    }

    Ok(())
}
