//! Filesystem host functions.

use std::path::Path;

use crate::host::HostState;

impl HostState {
    /// Validate that a path string is within the allowed directories.
    pub(super) fn check_path_allowed(&self, path_str: &str) -> Result<(), String> {
        let path = Path::new(path_str);
        let canonical = std::fs::canonicalize(path).map_err(|e| {
            format!(
                "cannot resolve path '{}' for plugin '{}': {e}",
                path_str, self.plugin_name
            )
        })?;
        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed_dir| canonical.starts_with(allowed_dir));
        if !allowed {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}'",
                path_str, self.plugin_name
            ));
        }
        Ok(())
    }

    /// Write content to a file (sandboxed to allowed paths).
    ///
    /// Canonicalizes the parent directory (since the file may not exist
    /// yet) and verifies it is within the allowed paths.
    pub fn write_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
        self.require_filesystem_capability("file writing")?;
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                path, self.plugin_name
            ));
        }

        let file_path = Path::new(path);
        let parent = file_path.parent().ok_or_else(|| {
            format!(
                "path '{}' has no parent directory for plugin '{}'",
                path, self.plugin_name
            )
        })?;
        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
            format!(
                "cannot resolve parent of '{}' for plugin '{}': {e}",
                path, self.plugin_name
            )
        })?;

        let file_name = file_path
            .file_name()
            .ok_or_else(|| format!("path '{}' has no filename", path))?;
        let canonical_target = canonical_parent.join(file_name);

        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed_dir| canonical_target.starts_with(allowed_dir));
        if !allowed {
            return Err(format!(
                "path '{}' is not within allowed directories for plugin '{}'",
                path, self.plugin_name
            ));
        }

        std::fs::write(file_path, content).map_err(|e| format!("failed to write '{}': {e}", path))
    }

    /// Read filesystem metadata for an allowed path and serialize it as MessagePack JSON.
    pub fn read_file_metadata(&self, path: &str) -> Result<Vec<u8>, String> {
        self.require_filesystem_capability("file metadata reads")?;
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "path '{path}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                self.plugin_name
            ));
        }
        self.check_path_allowed(path)?;

        let file_path = Path::new(path);
        let meta = std::fs::metadata(file_path)
            .map_err(|e| format!("failed to read metadata for '{path}': {e}"))?;
        let info = serde_json::json!({
            "size": meta.len(),
            "is_file": meta.is_file(),
            "is_dir": meta.is_dir(),
            "readonly": meta.permissions().readonly(),
            "modified": meta.modified().ok().map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            }),
        });
        rmp_serde::to_vec(&info).map_err(|e| format!("failed to serialize metadata: {e}"))
    }

    /// List files in an allowed directory whose names contain `pattern`.
    pub fn list_files(&self, dir: &str, pattern: &str) -> Result<Vec<String>, String> {
        self.require_filesystem_capability("directory listing")?;
        if self.allowed_paths.is_empty() {
            return Err(format!(
                "directory '{dir}' is not within allowed directories for plugin '{}' \
                 (no paths configured)",
                self.plugin_name
            ));
        }
        self.check_path_allowed(dir)?;

        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("failed to list directory '{dir}': {e}"))?;
        let mut files = Vec::new();
        for entry_result in entries {
            let entry = entry_result.map_err(|e| {
                format!(
                    "failed to read directory entry while listing '{dir}' for plugin '{}': {e}",
                    self.plugin_name
                )
            })?;
            let file_name = entry.file_name();
            if pattern.is_empty() || file_name.to_string_lossy().contains(pattern) {
                files.push(file_name.to_string_lossy().to_string());
            }
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::host::HostState;

    fn state_with_capability(kind: &str) -> HostState {
        HostState::new("test".into()).with_capabilities(HashSet::from([kind.to_string()]))
    }

    #[test]
    fn test_write_file_allowed_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = state_with_capability("discover").with_paths(vec![canonical_dir.clone()]);
        let file_path = canonical_dir.join("output.srt");
        let result = state.write_file(
            &file_path.to_string_lossy(),
            b"1\n00:00:00,000 --> 00:00:02,500\nHello\n",
        );
        assert!(result.is_ok());
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "1\n00:00:00,000 --> 00:00:02,500\nHello\n"
        );
    }

    #[test]
    fn test_write_file_blocked_path() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = state_with_capability("discover").with_paths(vec![canonical_dir]);
        let result = state.write_file("/etc/evil.txt", b"bad");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not within allowed"));
    }

    #[test]
    fn test_write_file_no_paths_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_capability("discover");
        let file_path = dir.path().join("output.txt");
        let result = state.write_file(&file_path.to_string_lossy(), b"hello");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not within allowed"));
    }

    #[test]
    fn test_write_file_no_filename_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = state_with_capability("discover").with_paths(vec![canonical_dir.clone()]);
        let path_str = format!("{}/..", canonical_dir.to_string_lossy());
        let result = state.write_file(&path_str, b"data");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("no filename"),
            "expected filename error, got: {err}"
        );
    }

    #[test]
    fn test_write_file_unresolvable_parent_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path()).unwrap();
        let state = state_with_capability("discover").with_paths(vec![canonical_dir]);
        let result = state.write_file("/definitely/does/not/exist/file.txt", b"data");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("cannot resolve parent"),
            "expected parent-resolution error, got: {err}"
        );
    }
}
