//! Filesystem discovery plugin: parallel directory walking with content hashing.

pub mod scanner;

pub use scanner::hash_file;

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::FileDiscoveredEvent;
use voom_kernel::Plugin;

/// Progress update during a scan.
#[derive(Debug, Clone)]
pub enum ScanProgress {
    /// Discovery phase: found a file during directory walk.
    Discovered {
        count: usize,
        path: std::path::PathBuf,
    },
    /// Processing phase: hashing/building event for a file.
    Processing {
        current: usize,
        total: usize,
        path: std::path::PathBuf,
    },
}

/// Configuration for a discovery scan.
#[non_exhaustive]
pub struct ScanOptions {
    /// Root directory to scan.
    pub root: std::path::PathBuf,
    /// Whether to recurse into subdirectories.
    pub recursive: bool,
    /// Whether to compute content hashes (xxHash64).
    pub hash_files: bool,
    /// Number of parallel workers for hashing (0 = auto).
    pub workers: usize,
    /// Optional progress callback.
    pub on_progress: Option<Box<dyn Fn(ScanProgress) + Send + Sync>>,
}

impl ScanOptions {
    #[must_use]
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            root: root.into(),
            recursive: true,
            hash_files: true,
            workers: 0,
            on_progress: None,
        }
    }
}

impl std::fmt::Debug for ScanOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanOptions")
            .field("root", &self.root)
            .field("recursive", &self.recursive)
            .field("hash_files", &self.hash_files)
            .field("workers", &self.workers)
            .field("on_progress", &self.on_progress.as_ref().map(|_| "..."))
            .finish()
    }
}

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
    fn name(&self) -> &str {
        "discovery"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::events::Event;

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
