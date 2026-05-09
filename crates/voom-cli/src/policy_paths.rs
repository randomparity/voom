//! Policy file location helpers.

use std::path::{Path, PathBuf};

use crate::config_paths::voom_config_dir;

/// Path to the default policies directory.
pub fn policies_dir() -> PathBuf {
    voom_config_dir().join("policies")
}

/// Resolve a policy path: use as-is if it exists, otherwise check the
/// default policies directory. Returns the original path unchanged if
/// neither location has the file (so the caller produces a normal
/// "not found" error).
pub fn resolve_policy_path(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    // Only fall back for bare filenames (no directory component).
    if path.parent().is_some_and(|p| p != Path::new("")) {
        return path.to_path_buf();
    }
    let candidate = policies_dir().join(path);
    if candidate.exists() {
        return candidate;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn test_resolve_policy_path_existing_file_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("local.voom");
        std::fs::write(&file, "").unwrap();

        let resolved = resolve_policy_path(&file);
        assert_eq!(resolved, file);
    }

    #[test]
    fn test_resolve_policy_path_falls_back_to_policies_dir() {
        let pdir = policies_dir();
        std::fs::create_dir_all(&pdir).ok();
        let policy_file = pdir.join("_test_resolve_fallback.voom");
        std::fs::write(&policy_file, "").unwrap();

        let resolved = resolve_policy_path(Path::new("_test_resolve_fallback.voom"));
        assert_eq!(resolved, policy_file);

        std::fs::remove_file(&policy_file).ok();
    }

    #[test]
    fn test_resolve_policy_path_no_fallback_for_paths_with_dirs() {
        let resolved = resolve_policy_path(Path::new("subdir/missing.voom"));
        assert_eq!(resolved, Path::new("subdir/missing.voom"));
    }

    #[test]
    fn test_resolve_policy_path_returns_original_when_not_found() {
        let resolved = resolve_policy_path(Path::new("nonexistent_xyzzy.voom"));
        assert_eq!(resolved, Path::new("nonexistent_xyzzy.voom"));
    }
}
