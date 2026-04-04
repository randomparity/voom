use std::path::PathBuf;

use anyhow::{Context, Result};

/// Canonicalize paths and deduplicate, removing paths that are subdirectories
/// of another provided path (to avoid scanning overlapping trees).
pub fn resolve_paths(raw: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut canonical: Vec<PathBuf> = Vec::with_capacity(raw.len());
    for p in raw {
        let c = p
            .canonicalize()
            .with_context(|| format!("Path not found: {}", p.display()))?;
        canonical.push(c);
    }
    canonical.sort();
    canonical.dedup();

    let mut filtered: Vec<PathBuf> = Vec::with_capacity(canonical.len());
    for path in &canonical {
        let dominated = filtered.iter().any(|existing| path.starts_with(existing));
        if !dominated {
            filtered.retain(|existing| !existing.starts_with(path));
            filtered.push(path.clone());
        }
    }
    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_identical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_path_buf();
        let result = resolve_paths(&[p.clone(), p.clone()]).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_child_filtered_by_parent() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).unwrap();
        let result = resolve_paths(&[dir.path().to_path_buf(), child]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_independent_paths_kept() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let result =
            resolve_paths(&[dir1.path().to_path_buf(), dir2.path().to_path_buf()]).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_nonexistent_path_errors() {
        let result = resolve_paths(&[PathBuf::from("/nonexistent/path/abc123")]);
        assert!(result.is_err());
    }
}
