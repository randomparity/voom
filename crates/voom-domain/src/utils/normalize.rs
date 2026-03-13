/// Normalize a track title by trimming whitespace and collapsing runs of whitespace.
#[must_use] 
pub fn normalize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize a file extension by lowercasing and stripping the leading dot.
#[must_use] 
pub fn normalize_extension(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

/// Normalize a tag key for consistent lookups (lowercase, trim whitespace).
#[must_use] 
pub fn normalize_tag_key(key: &str) -> String {
    key.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_title() {
        assert_eq!(normalize_title("  Hello   World  "), "Hello World");
        assert_eq!(normalize_title("Already Fine"), "Already Fine");
        assert_eq!(normalize_title(""), "");
        assert_eq!(normalize_title("  "), "");
    }

    #[test]
    fn test_normalize_extension() {
        assert_eq!(normalize_extension(".MKV"), "mkv");
        assert_eq!(normalize_extension("MP4"), "mp4");
        assert_eq!(normalize_extension(".m2ts"), "m2ts");
    }

    #[test]
    fn test_normalize_tag_key() {
        assert_eq!(normalize_tag_key("  TITLE  "), "title");
        assert_eq!(normalize_tag_key("Artist"), "artist");
    }
}
