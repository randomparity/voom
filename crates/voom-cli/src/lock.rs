//! Exclusive process lock preventing concurrent mutating operations.
//!
//! Acquires a file-based lock on `<data_dir>/voom.lock` using `flock(2)`.
//! The lock is released automatically when the `ProcessLock` is dropped.

use std::fs::{self, File};
use std::path::Path;

use anyhow::{Context, Result};
use fs2::FileExt;

/// Holds an exclusive flock on `<data_dir>/voom.lock` for the process lifetime.
#[derive(Debug)]
pub struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    /// Acquire an exclusive process lock under `data_dir`.
    ///
    /// Creates `data_dir` (and any parents) if it does not exist, then
    /// creates or opens `<data_dir>/voom.lock` and calls `try_lock_exclusive`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The directory cannot be created.
    /// - The lock file cannot be opened.
    /// - Another process already holds the lock.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;

        let path = data_dir.join("voom.lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("Failed to open lock file: {}", path.display()))?;

        file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "Another voom process is running (lock held on {}). Use --force to override.",
                path.display()
            )
        })?;

        Ok(Self { _file: file })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self._file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // flock(2) is per open-file-description, but within a single process
    // concurrent threads can interfere when multiple tests acquire/release
    // locks simultaneously. Serialize all lock tests to prevent flakes.
    static LOCK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_test_guard() -> std::sync::MutexGuard<'static, ()> {
        LOCK_TEST
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn test_acquire_succeeds_in_fresh_dir() {
        let _guard = lock_test_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let _lock = ProcessLock::acquire(dir.path()).expect("acquire");
        assert!(dir.path().join("voom.lock").exists());
    }

    #[test]
    fn test_second_acquire_fails() {
        let _guard = lock_test_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let _first = ProcessLock::acquire(dir.path()).expect("first acquire");
        let result = ProcessLock::acquire(dir.path());
        assert!(
            result.is_err(),
            "second acquire should fail while first is held"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Another voom process is running"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("--force"),
            "error should mention --force: {msg}"
        );
    }

    #[test]
    fn test_lock_released_on_drop() {
        let _guard = lock_test_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let _lock = ProcessLock::acquire(dir.path()).expect("first acquire");
        }
        // First lock is dropped; acquiring again must succeed.
        let _lock = ProcessLock::acquire(dir.path()).expect("re-acquire after drop");
    }

    #[test]
    fn test_creates_nested_dirs() {
        let _guard = lock_test_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("c");
        assert!(!nested.exists());
        let _lock = ProcessLock::acquire(&nested).expect("acquire in nested dir");
        assert!(nested.exists(), "nested dirs should have been created");
    }

    #[test]
    fn test_lock_file_is_named_voom_lock() {
        let _guard = lock_test_guard();
        let dir = tempfile::tempdir().expect("tempdir");
        let _lock = ProcessLock::acquire(dir.path()).expect("acquire");
        assert!(dir.path().join("voom.lock").exists());
    }
}
