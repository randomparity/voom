//! Disk space utilities.

use std::path::Path;

use crate::errors::{Result, VoomError};

/// Get available disk space for a path (bytes).
///
/// Walks up to the nearest existing ancestor if the path doesn't exist yet.
#[cfg(unix)]
pub fn available_space(path: &Path) -> Result<u64> {
    let mut check = path.to_path_buf();
    while !check.exists() {
        match check.parent() {
            Some(p) => check = p.to_path_buf(),
            None => break,
        }
    }

    use std::ffi::CString;
    let c_path = CString::new(check.to_string_lossy().as_bytes())
        .map_err(|e| VoomError::Validation(format!("invalid path for statvfs: {e}")))?;

    // SAFETY: `c_path` is a valid NUL-terminated C string (from CString::new
    // above), and `stat` is passed as an out-pointer that statvfs will fully
    // initialize on success.
    unsafe {
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) == 0 {
            let stat = stat.assume_init();
            #[allow(clippy::unnecessary_cast)]
            let avail = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
            Ok(avail)
        } else {
            Err(VoomError::Io(std::io::Error::last_os_error()))
        }
    }
}

/// On non-Unix platforms, return a large value to avoid blocking.
#[cfg(not(unix))]
pub fn available_space(_path: &Path) -> Result<u64> {
    Ok(u64::MAX)
}
