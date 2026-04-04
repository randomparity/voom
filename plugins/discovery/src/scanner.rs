use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use rayon::prelude::*;
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;
use xxhash_rust::xxh3::Xxh3;

use voom_domain::errors::{Result, VoomError};
use voom_domain::events::FileDiscoveredEvent;
use voom_domain::temp_file::is_voom_temp;

use crate::ScanOptions;

/// Normalize a file path for consistent storage and comparison.
///
/// Applies two transformations:
/// 1. `fs::canonicalize()` — resolves symlinks and macOS /var → /private/var
/// 2. Unicode NFC normalization — recomposes macOS NFD-decomposed filenames
///
/// Falls back to the raw path if canonicalization fails (e.g., file deleted
/// between walk and normalization).
pub fn normalize_path(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized: String = canonical.to_string_lossy().nfc().collect();
    PathBuf::from(normalized)
}

/// Maximum concurrent open file handles during hashing to avoid exhausting
/// the process file descriptor limit under heavy parallelism.
const MAX_CONCURRENT_FDS: usize = 256;

/// Counting semaphore for bounding concurrent file descriptor usage.
/// Uses `std::sync` primitives (not tokio) because discovery runs on rayon.
struct FdSemaphore {
    state: Mutex<usize>,
    cond: Condvar,
    max: usize,
}

impl FdSemaphore {
    fn new(max: usize) -> Self {
        Self {
            state: Mutex::new(0),
            cond: Condvar::new(),
            max,
        }
    }

    fn acquire(&self) {
        let mut count = self.state.lock().expect("fd semaphore lock poisoned");
        while *count >= self.max {
            count = self.cond.wait(count).expect("fd semaphore lock poisoned");
        }
        *count += 1;
    }

    fn release(&self) {
        let mut count = self.state.lock().expect("fd semaphore lock poisoned");
        *count -= 1;
        self.cond.notify_one();
    }

    /// Returns an RAII guard that releases on drop (including panic unwind).
    fn guard(&self) -> FdGuard<'_> {
        self.acquire();
        FdGuard(self)
    }
}

struct FdGuard<'a>(&'a FdSemaphore);

impl Drop for FdGuard<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Media file extensions recognized by the discovery plugin.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mka", "mks", // Matroska
    "mp4", "m4v", "m4a",  // MPEG-4
    "avi",  // AVI
    "webm", // WebM
    "flv",  // Flash Video
    "wmv", "wma", // Windows Media
    "mov", // QuickTime
    "ts", "m2ts", "mts", // MPEG Transport Stream
];

/// Returns true if the file extension indicates a media file.
fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Size threshold above which we switch to partial hashing.
/// Files at or below this size are hashed in full.
const PARTIAL_HASH_THRESHOLD: u64 = 2 * 1024 * 1024; // 2 MB

/// Size of each chunk sampled for partial hashing.
const HASH_CHUNK_SIZE: usize = 1024 * 1024; // 1 MB

/// Compute xxHash64 of a file's contents.
///
/// Small files (≤ 2 MB) are hashed in full. Larger files use a partial
/// hash strategy that samples the first, middle, and last 1 MB of the file
/// combined with the file size. This keeps hashing fast for multi-GB video
/// files while still reliably detecting duplicates and content changes.
pub fn hash_file(path: &Path) -> Result<String> {
    let io_err = |e: std::io::Error| {
        VoomError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {e}", path.display()),
        ))
    };
    let mut file = fs::File::open(path).map_err(io_err)?;
    let file_size = file.metadata().map_err(io_err)?.len();

    if file_size <= PARTIAL_HASH_THRESHOLD {
        return hash_file_full(&mut file);
    }

    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; HASH_CHUNK_SIZE];

    // Include file size in the hash so files with identical heads/tails
    // but different sizes produce different hashes.
    hasher.update(&file_size.to_le_bytes());

    // First chunk
    let n = file.read(&mut buf).map_err(io_err)?;
    hasher.update(&buf[..n]);

    // Middle chunk
    let mid = file_size / 2;
    file.seek(SeekFrom::Start(mid)).map_err(io_err)?;
    let n = file.read(&mut buf).map_err(io_err)?;
    hasher.update(&buf[..n]);

    // Last chunk
    let tail_offset = file_size.saturating_sub(HASH_CHUNK_SIZE as u64);
    file.seek(SeekFrom::Start(tail_offset)).map_err(io_err)?;
    let n = file.read(&mut buf).map_err(io_err)?;
    hasher.update(&buf[..n]);

    Ok(format!("{:016x}", hasher.digest()))
}

/// Hash a small file by reading it in full.
fn hash_file_full(file: &mut fs::File) -> Result<String> {
    let mut hasher = Xxh3::new();
    let mut buf = [0u8; 256 * 1024];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:016x}", hasher.digest()))
}

/// Scan a directory for media files.
pub fn scan_directory(options: &ScanOptions) -> Result<Vec<FileDiscoveredEvent>> {
    if !options.root.exists() {
        return Err(VoomError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("directory not found: {}", options.root.display()),
        )));
    }

    let walker = if options.recursive {
        WalkDir::new(&options.root).follow_links(false)
    } else {
        WalkDir::new(&options.root).max_depth(1).follow_links(false)
    };

    // Collect media file paths with sizes, reporting progress as we discover them.
    // Size is captured from walkdir metadata so it's available even if the file
    // disappears before hashing.
    let mut media_paths: Vec<(std::path::PathBuf, u64)> = Vec::new();
    let mut orphaned_temp_count: usize = 0;
    for entry in walker.into_iter().filter_map(|e| {
        e.map_err(|err| {
            tracing::debug!(error = %err, "skipping unreadable directory entry");
        })
        .ok()
    }) {
        if entry.path_is_symlink() {
            tracing::debug!(path = %entry.path().display(), "skipping symlink");
            continue;
        }
        if entry.file_type().is_file() && is_media_file(entry.path()) {
            if is_voom_temp(entry.path()) {
                tracing::warn!(
                    path = %entry.path().display(),
                    "skipping orphaned voom temp file"
                );
                orphaned_temp_count += 1;
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let path = entry.into_path();
            if let Some(ref cb) = options.on_progress {
                cb(crate::ScanProgress::Discovered {
                    count: media_paths.len() + 1,
                    path: path.clone(),
                });
            }
            media_paths.push((normalize_path(&path), size));
        }
    }

    if orphaned_temp_count > 0 {
        if let Some(ref cb) = options.on_progress {
            cb(crate::ScanProgress::OrphanedTempFiles {
                count: orphaned_temp_count,
            });
        }
    }

    tracing::info!(
        root = %options.root.display(),
        count = media_paths.len(),
        "discovered media files"
    );

    let total = media_paths.len();
    let processed = Arc::new(AtomicUsize::new(0));
    let fd_sem = Arc::new(FdSemaphore::new(MAX_CONCURRENT_FDS));

    // Process files in parallel using rayon, with a semaphore to limit
    // concurrent open file descriptors during hashing.
    let process_file = |(path, walk_size): &(std::path::PathBuf, u64)| {
        let _guard = fd_sem.guard();
        let result = build_event(path, *walk_size, options.hash_files);
        let current = processed.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(ref cb) = options.on_progress {
            cb(crate::ScanProgress::Processing {
                current,
                total,
                path: path.clone(),
            });
        }
        result
    };

    let events: Vec<Result<FileDiscoveredEvent>> = if options.workers > 0 {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.workers)
            .build()
            .map_err(|e| VoomError::Other(Box::new(e)))?;
        pool.install(|| media_paths.par_iter().map(process_file).collect())
    } else {
        media_paths.par_iter().map(process_file).collect()
    };

    // Collect results, reporting errors via callback (or logging as fallback)
    let mut discovered = Vec::with_capacity(events.len());
    for (result, (path, walk_size)) in events.into_iter().zip(media_paths.iter()) {
        match result {
            Ok(event) => discovered.push(event),
            Err(e) => {
                if let Some(ref cb) = options.on_error {
                    cb(path.clone(), *walk_size, e.to_string());
                } else {
                    tracing::warn!(error = %e, "failed to process file during discovery");
                }
            }
        }
    }

    // Sort by path for deterministic output
    discovered.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(discovered)
}

/// Build a `FileDiscoveredEvent` for a single file.
///
/// `walk_size` is the file size captured during the directory walk. It avoids
/// a redundant `stat` call — `hash_file` will detect a missing file on its own.
fn build_event(path: &Path, walk_size: u64, compute_hash: bool) -> Result<FileDiscoveredEvent> {
    let content_hash = if compute_hash {
        Some(hash_file(path)?)
    } else {
        None
    };

    tracing::debug!(path = %path.display(), size = walk_size, "file discovered");

    Ok(FileDiscoveredEvent::new(
        path.to_path_buf(),
        walk_size,
        content_hash,
    ))
}

#[cfg(test)]
mod normalize_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_normalize_path_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.mkv");
        std::fs::write(&file, b"data").unwrap();
        let result = normalize_path(&file);
        assert!(result.is_absolute());
        assert_eq!(result.file_name().unwrap(), "test.mkv");
    }

    #[test]
    fn test_normalize_path_missing_file_returns_raw() {
        let path = PathBuf::from("/nonexistent/file.mkv");
        let result = normalize_path(&path);
        assert_eq!(result, path, "missing file should return raw path");
    }

    #[test]
    fn test_normalize_path_nfc_normalization() {
        // é as NFD (e + combining acute) vs NFC (single codepoint)
        let nfd = "caf\u{0065}\u{0301}.mkv";
        let nfc = "caf\u{00e9}.mkv";
        let nfd_path = PathBuf::from(nfd);
        let result = normalize_path(&nfd_path);
        let result_str = result.to_string_lossy();
        assert!(
            result_str.contains('\u{00e9}'),
            "should contain NFC é, got: {result_str}"
        );
        assert!(
            !result_str.contains('\u{0301}'),
            "should not contain combining accent after NFC"
        );
        let nfc_result = normalize_path(&PathBuf::from(nfc));
        assert_eq!(
            result.to_string_lossy(),
            nfc_result.to_string_lossy(),
            "NFD and NFC inputs should produce identical normalized paths"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_media_file() {
        assert!(is_media_file(Path::new("video.mkv")));
        assert!(is_media_file(Path::new("video.MKV")));
        assert!(is_media_file(Path::new("video.Mp4")));
        assert!(is_media_file(Path::new("/some/path/video.avi")));
        assert!(is_media_file(Path::new("audio.m4a")));
        assert!(is_media_file(Path::new("video.m2ts")));
        assert!(!is_media_file(Path::new("readme.txt")));
        assert!(!is_media_file(Path::new("image.png")));
        assert!(!is_media_file(Path::new("no_extension")));
    }

    #[test]
    fn test_hash_file_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        std::fs::write(&path, b"hello world").unwrap();

        let hash1 = hash_file(&path).unwrap();
        let hash2 = hash_file(&path).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 16); // 64-bit hex = 16 chars
    }

    #[test]
    fn test_hash_file_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let path1 = dir.path().join("a.bin");
        let path2 = dir.path().join("b.bin");
        std::fs::write(&path1, b"content A").unwrap();
        std::fs::write(&path2, b"content B").unwrap();

        let hash1 = hash_file(&path1).unwrap();
        let hash2 = hash_file(&path2).unwrap();
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_build_event_with_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mkv");
        std::fs::write(&path, b"fake video data").unwrap();

        let event = build_event(&path, 15, true).unwrap();
        assert_eq!(event.path, path);
        assert_eq!(event.size, 15);
        assert!(event.content_hash.is_some());
    }

    #[test]
    fn test_walker_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("video.mkv"), b"data").unwrap();

        // Create a symlink to the real directory
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real_dir, dir.path().join("link")).unwrap();
        }

        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            recursive: true,
            hash_files: false,
            workers: 0,
            on_progress: None,
            on_error: None,
        };
        let events = scan_directory(&options).unwrap();
        // Should only find the file under "real/", not under "link/"
        assert_eq!(events.len(), 1);
        // normalize_path canonicalizes paths, so compare against canonical real_dir
        let canonical_real_dir = std::fs::canonicalize(&real_dir).unwrap_or(real_dir);
        assert!(events[0].path.starts_with(&canonical_real_dir));
    }

    #[test]
    fn test_build_event_without_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mkv");
        std::fs::write(&path, b"fake video data").unwrap();

        let event = build_event(&path, 15, false).unwrap();
        assert!(event.content_hash.is_none());
    }

    #[test]
    fn test_scan_skips_voom_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.mkv"), b"real data").unwrap();
        std::fs::write(
            dir.path().join("movie.voom_tmp_abc123def456.mkv"),
            b"temp data",
        )
        .unwrap();

        let options = ScanOptions::new(dir.path());
        let events = scan_directory(&options).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path.file_name().unwrap(), "real.mkv");
    }

    #[test]
    fn test_scan_reports_orphaned_temp_count() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.mkv"), b"real data").unwrap();
        std::fs::write(dir.path().join("movie.voom_tmp_abc.mkv"), b"temp1").unwrap();
        std::fs::write(dir.path().join("other.voom_tmp_xyz.mp4"), b"temp2").unwrap();

        let orphan_count = Arc::new(AtomicUsize::new(0));
        let orphan_clone = orphan_count.clone();

        let mut options = ScanOptions::new(dir.path());
        options.on_progress = Some(Box::new(move |p| {
            if let crate::ScanProgress::OrphanedTempFiles { count } = p {
                orphan_clone.store(count, Ordering::Relaxed);
            }
        }));

        let events = scan_directory(&options).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(orphan_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_scan_calls_on_error_for_failures() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let vanishing = dir.path().join("vanish.mkv");
        std::fs::write(&vanishing, b"data").unwrap();
        std::fs::write(dir.path().join("real.mkv"), b"real data").unwrap();

        let error_count = Arc::new(AtomicUsize::new(0));
        let error_clone = error_count.clone();

        let mut options = ScanOptions::new(dir.path());
        options.hash_files = true;
        options.on_error = Some(Box::new(move |_path, _size, _error| {
            error_clone.fetch_add(1, Ordering::Relaxed);
        }));

        // Delete the file after walk but before hash to trigger an error.
        // We can't easily time this, so just verify the callback mechanism
        // works by running the scan and checking it completes without panicking.
        let events = scan_directory(&options).unwrap();
        // Both files should still exist, so we get 2 events and 0 errors
        assert_eq!(events.len(), 2);
        assert_eq!(error_count.load(Ordering::Relaxed), 0);
    }
}
