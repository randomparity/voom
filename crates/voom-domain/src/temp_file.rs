use std::path::{Path, PathBuf};

/// Marker string embedded in voom temp file names.
pub const TEMP_MARKER: &str = ".voom_tmp_";

/// Generate a temp file path for the given original file.
///
/// Pattern: `{stem}.voom_tmp_{uuid}.{ext}`
/// Placed in the same directory as the original.
#[must_use]
pub fn temp_path(original: &Path) -> PathBuf {
    let ext = original
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mkv");
    temp_path_with_ext(original, ext)
}

/// Generate a temp file path with a specific extension (for container conversion).
///
/// Pattern: `{stem}.voom_tmp_{uuid}.{ext}`
#[must_use]
pub fn temp_path_with_ext(original: &Path, ext: &str) -> PathBuf {
    let parent = original.parent().unwrap_or_else(|| Path::new("."));
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let id = uuid::Uuid::new_v4().as_simple().to_string();
    parent.join(format!("{stem}{TEMP_MARKER}{id}.{ext}"))
}

/// Check if a path is a voom temp file (filename contains [`TEMP_MARKER`]).
#[must_use]
pub fn is_voom_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.contains(TEMP_MARKER))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_path_preserves_extension() {
        let p = temp_path(Path::new("/media/movie.mkv"));
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("mkv"));
    }

    #[test]
    fn temp_path_stays_in_same_directory() {
        let p = temp_path(Path::new("/media/sub/movie.mp4"));
        assert_eq!(p.parent(), Some(Path::new("/media/sub")));
    }

    #[test]
    fn temp_path_contains_marker() {
        let p = temp_path(Path::new("/media/movie.mkv"));
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.contains(TEMP_MARKER));
    }

    #[test]
    fn temp_path_unique_per_call() {
        let a = temp_path(Path::new("/media/movie.mkv"));
        let b = temp_path(Path::new("/media/movie.mkv"));
        assert_ne!(a, b);
    }

    #[test]
    fn temp_path_with_ext_uses_given_extension() {
        let p = temp_path_with_ext(Path::new("/media/movie.mkv"), "mp4");
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("mp4"));
        assert!(is_voom_temp(&p));
    }

    #[test]
    fn is_voom_temp_matches_temp_files() {
        assert!(is_voom_temp(Path::new("/media/movie.voom_tmp_abc123.mkv")));
        assert!(is_voom_temp(Path::new("file.voom_tmp_xyz.mp4")));
    }

    #[test]
    fn is_voom_temp_rejects_normal_files() {
        assert!(!is_voom_temp(Path::new("/media/movie.mkv")));
        assert!(!is_voom_temp(Path::new("/media/movie.tmp.mkv")));
        assert!(!is_voom_temp(Path::new("readme.txt")));
    }

    #[test]
    fn temp_path_roundtrips_through_is_voom_temp() {
        let p = temp_path(Path::new("/media/movie.mkv"));
        assert!(is_voom_temp(&p));
    }
}
