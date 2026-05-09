//! Policy file location helpers.

use std::path::{Path, PathBuf};

use crate::config::voom_config_dir;

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
