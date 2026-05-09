//! Filesystem discovery plugin: parallel directory walking with content hashing.

pub mod scanner;

pub use scanner::{
    hash_file, normalize_path, ErrorCallback, FingerprintLookup, ScanOptions, ScanProgress,
};

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::FileDiscoveredEvent;
use voom_kernel::Plugin;

/// Discovery plugin: walks the filesystem to find media files.
///
/// Uses walkdir for traversal and rayon for parallel content hashing.
/// Emits `FileDiscovered` events for each media file found.
pub struct DiscoveryPlugin {
    capabilities: Vec<Capability>,
}

impl DiscoveryPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Discover {
                schemes: vec!["file".into()],
            }],
        }
    }

    /// Scan a directory for media files and return discovery events.
    pub fn scan(&self, options: &ScanOptions) -> Result<Vec<FileDiscoveredEvent>> {
        scanner::scan_directory(options)
    }
}

impl Default for DiscoveryPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for DiscoveryPlugin {
    fn name(&self) -> &'static str {
        "discovery"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use voom_domain::events::Event;
    use voom_kernel::Plugin;

    use crate::{normalize_path, DiscoveryPlugin, ScanOptions, ScanProgress};

    #[test]
    fn test_plugin_metadata() {
        let plugin = DiscoveryPlugin::new();
        assert_eq!(plugin.name(), "discovery");
        assert!(!plugin.capabilities().is_empty());
        assert_eq!(plugin.capabilities()[0].kind(), "discover");
    }

    #[test]
    fn test_handles_no_events() {
        let plugin = DiscoveryPlugin::new();
        assert!(!plugin.handles(Event::FILE_DISCOVERED));
        assert!(!plugin.handles(Event::FILE_INTROSPECTED));
    }

    #[test]
    fn test_scan_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let opts = ScanOptions::new(dir.path());
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_scan_finds_media_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create some test files
        std::fs::write(dir.path().join("video.mkv"), b"fake mkv data").unwrap();
        std::fs::write(dir.path().join("audio.mp4"), b"fake mp4 data").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"not media").unwrap();
        std::fs::write(dir.path().join("image.jpg"), b"not media").unwrap();

        let opts = ScanOptions::new(dir.path());
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();

        assert_eq!(events.len(), 2);
        let paths: Vec<_> = events.iter().map(|e| e.path.file_name().unwrap()).collect();
        assert!(paths.contains(&std::ffi::OsStr::new("video.mkv")));
        assert!(paths.contains(&std::ffi::OsStr::new("audio.mp4")));
    }

    #[test]
    fn test_scan_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("top.mkv"), b"top").unwrap();
        std::fs::write(sub.join("nested.avi"), b"nested").unwrap();

        let opts = ScanOptions::new(dir.path());
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_scan_non_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("top.mkv"), b"top").unwrap();
        std::fs::write(sub.join("nested.avi"), b"nested").unwrap();

        let mut opts = ScanOptions::new(dir.path());
        opts.recursive = false;
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path.file_name().unwrap(), "top.mkv");
    }

    #[test]
    fn test_scan_with_hashing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.mkv"), b"test content for hashing").unwrap();

        let opts = ScanOptions::new(dir.path());
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();

        assert_eq!(events.len(), 1);
        assert!(events[0].content_hash.is_some());
        assert!(events[0].size > 0);
    }

    #[test]
    fn test_scan_without_hashing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test.mkv"), b"test content").unwrap();

        let mut opts = ScanOptions::new(dir.path());
        opts.hash_files = false;
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();

        assert_eq!(events.len(), 1);
        assert!(events[0].content_hash.is_none());
    }

    #[test]
    fn test_scan_nonexistent_dir() {
        let opts = ScanOptions::new("/nonexistent/path/that/should/not/exist");
        let plugin = DiscoveryPlugin::new();
        let result = plugin.scan(&opts);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_reuses_cached_hash_when_fingerprint_matches() {
        use chrono::Utc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();

        let canonical = normalize_path(&file_path);
        let file_size = std::fs::metadata(&file_path).unwrap().len();
        let cached_hash = "cached-hash-sentinel".to_string();

        // Fingerprint "last seen" is in the future so the on-disk mtime is
        // guaranteed to be earlier.
        let stored = StoredFingerprint::new(
            file_size,
            cached_hash.clone(),
            Utc::now() + chrono::Duration::hours(1),
        );

        let reused = Arc::new(AtomicUsize::new(0));
        let reused_clone = reused.clone();

        let mut opts = ScanOptions::new(dir.path());
        let stored_clone = stored.clone();
        let canonical_lookup = canonical.clone();
        opts.fingerprint_lookup = Some(Box::new(move |p| {
            if p == canonical_lookup {
                Some(stored_clone.clone())
            } else {
                None
            }
        }));
        opts.on_progress = Some(Box::new(move |p| {
            if let ScanProgress::HashReused { .. } = p {
                reused_clone.fetch_add(1, Ordering::Relaxed);
            }
        }));

        let events = DiscoveryPlugin::new().scan(&opts).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].content_hash.as_deref(),
            Some(cached_hash.as_str())
        );
        assert_eq!(reused.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_scan_rehashes_when_size_differs() {
        use chrono::Utc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();

        let stored = StoredFingerprint::new(
            9_999_999, // deliberately wrong
            "stale-hash",
            Utc::now() + chrono::Duration::hours(1),
        );

        let reused = Arc::new(AtomicUsize::new(0));
        let reused_clone = reused.clone();

        let mut opts = ScanOptions::new(dir.path());
        opts.fingerprint_lookup = Some(Box::new(move |_| Some(stored.clone())));
        opts.on_progress = Some(Box::new(move |p| {
            if let ScanProgress::HashReused { .. } = p {
                reused_clone.fetch_add(1, Ordering::Relaxed);
            }
        }));

        let events = DiscoveryPlugin::new().scan(&opts).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].content_hash.is_some());
        assert_ne!(events[0].content_hash.as_deref(), Some("stale-hash"));
        assert_eq!(
            reused.load(Ordering::Relaxed),
            0,
            "HashReused must not fire when size differs"
        );
    }

    #[test]
    fn test_scan_rehashes_when_mtime_is_newer_than_last_seen() {
        use chrono::Utc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();
        let file_size = std::fs::metadata(&file_path).unwrap().len();

        let stored = StoredFingerprint::new(
            file_size,
            "stale-hash".to_string(),
            Utc::now() - chrono::Duration::days(365),
        );

        let reused = Arc::new(AtomicUsize::new(0));
        let reused_clone = reused.clone();

        let mut opts = ScanOptions::new(dir.path());
        opts.fingerprint_lookup = Some(Box::new(move |_| Some(stored.clone())));
        opts.on_progress = Some(Box::new(move |p| {
            if let ScanProgress::HashReused { .. } = p {
                reused_clone.fetch_add(1, Ordering::Relaxed);
            }
        }));

        let events = DiscoveryPlugin::new().scan(&opts).unwrap();
        assert_eq!(events.len(), 1);
        assert_ne!(events[0].content_hash.as_deref(), Some("stale-hash"));
        assert_eq!(
            reused.load(Ordering::Relaxed),
            0,
            "HashReused must not fire when mtime is newer than last_seen"
        );
    }

    #[test]
    fn test_scan_all_supported_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let extensions = [
            "mkv", "mka", "mks", "mp4", "m4v", "m4a", "avi", "webm", "flv", "wmv", "wma", "mov",
            "ts", "m2ts", "mts",
        ];
        for ext in &extensions {
            std::fs::write(dir.path().join(format!("file.{ext}")), b"data").unwrap();
        }
        // Also non-media
        std::fs::write(dir.path().join("file.txt"), b"text").unwrap();

        let opts = ScanOptions::new(dir.path());
        let plugin = DiscoveryPlugin::new();
        let events = plugin.scan(&opts).unwrap();
        assert_eq!(events.len(), extensions.len());
    }
}
