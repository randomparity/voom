//! Quarantine handler — moves a file to a configured quarantine directory.

use std::path::{Path, PathBuf};

use voom_domain::errors::{Result, VoomError};

/// Move `src` into `quarantine_dir`, preserving the basename. If a file
/// already exists at the destination, append `.<n>` until unique.
///
/// Returns the final destination path on success.
///
/// Falls back to copy+delete if `rename` returns EXDEV (cross-device link).
///
/// # Errors
/// Returns an error if the source has no filename, the quarantine directory
/// can't be created/written, or no unique destination can be found in 9999
/// attempts.
pub fn quarantine_file(src: &Path, quarantine_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(quarantine_dir).map_err(|e| VoomError::ToolExecution {
        tool: "quarantine".into(),
        message: format!("create {}: {e}", quarantine_dir.display()),
    })?;

    let basename = src.file_name().ok_or_else(|| VoomError::ToolExecution {
        tool: "quarantine".into(),
        message: format!("source {} has no filename", src.display()),
    })?;

    let mut dst = quarantine_dir.join(basename);
    let mut suffix = 1u32;
    while dst.exists() {
        dst = quarantine_dir.join(format!("{}.{suffix}", basename.to_string_lossy()));
        suffix += 1;
        if suffix > 9999 {
            return Err(VoomError::ToolExecution {
                tool: "quarantine".into(),
                message: "could not find unique quarantine destination".into(),
            });
        }
    }

    match std::fs::rename(src, &dst) {
        Ok(()) => Ok(dst),
        Err(e) if cross_device_error(&e) => {
            std::fs::copy(src, &dst).map_err(|e| VoomError::ToolExecution {
                tool: "quarantine".into(),
                message: format!("copy {} -> {}: {e}", src.display(), dst.display()),
            })?;
            std::fs::remove_file(src).map_err(|e| VoomError::ToolExecution {
                tool: "quarantine".into(),
                message: format!("remove {}: {e}", src.display()),
            })?;
            Ok(dst)
        }
        Err(e) => Err(VoomError::ToolExecution {
            tool: "quarantine".into(),
            message: format!("rename {} -> {}: {e}", src.display(), dst.display()),
        }),
    }
}

fn cross_device_error(e: &std::io::Error) -> bool {
    // EXDEV is 18 on Linux/macOS. ErrorKind::CrossesDevices is unstable;
    // match on raw_os_error.
    matches!(e.raw_os_error(), Some(18))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn moves_file_to_quarantine() {
        let qd = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("bad.mkv");
        std::fs::File::create(&src)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let dst = quarantine_file(&src, qd.path()).unwrap();
        assert!(dst.exists());
        assert!(!src.exists());
        assert_eq!(dst.file_name().unwrap(), "bad.mkv");
    }

    #[test]
    fn appends_suffix_on_collision() {
        let qd = tempfile::tempdir().unwrap();
        std::fs::write(qd.path().join("dup.mkv"), b"existing").unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("dup.mkv");
        std::fs::write(&src, b"new").unwrap();
        let dst = quarantine_file(&src, qd.path()).unwrap();
        assert_eq!(dst.file_name().unwrap(), "dup.mkv.1");
    }

    #[test]
    fn missing_source_errors() {
        let qd = tempfile::tempdir().unwrap();
        let r = quarantine_file(Path::new("/nonexistent/file"), qd.path());
        assert!(r.is_err());
    }

    #[test]
    fn creates_quarantine_dir_if_missing() {
        let parent = tempfile::tempdir().unwrap();
        let qd = parent.path().join("nested/quarantine");
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("a.mkv");
        std::fs::write(&src, b"x").unwrap();
        let dst = quarantine_file(&src, &qd).unwrap();
        assert!(dst.exists());
        assert!(qd.is_dir());
    }
}
