//! Disk space validation.
//!
//! Checks that sufficient free space is available before creating a backup,
//! using `libc::statvfs` on Unix and a permissive fallback on other platforms.

use std::fs;
use std::path::Path;

use voom_domain::errors::{Result, VoomError};

use crate::plugin_err;

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
    let file_size = fs::metadata(source_path).map(|m| m.len()).unwrap_or(0);

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

/// Get available disk space for a path.
/// Walks up to the nearest existing ancestor if the path doesn't exist.
#[cfg(unix)]
pub fn available_space(path: &Path) -> Result<u64> {
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
    let c_path = CString::new(check.to_string_lossy().as_bytes())
        .map_err(|e| plugin_err(format!("invalid path for statvfs: {e}")))?;

    // SAFETY: `c_path` is a valid NUL-terminated C string (from CString::new above),
    // and `stat` is passed as an out-pointer that statvfs will fully initialize on success.
    unsafe {
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) == 0 {
            let stat = stat.assume_init();
            // Available space for unprivileged users.
            // f_bavail is u32 on macOS, u64 on Linux — cast needed for portability.
            #[allow(clippy::unnecessary_cast)]
            let avail = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
            Ok(avail)
        } else {
            Err(VoomError::Io(std::io::Error::last_os_error()))
        }
    }
}

#[cfg(not(unix))]
pub fn available_space(_path: &Path) -> Result<u64> {
    // On non-Unix platforms, return a large value to avoid blocking.
    Ok(u64::MAX)
}
