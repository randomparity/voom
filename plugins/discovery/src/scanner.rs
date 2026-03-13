use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rayon::prelude::*;
use walkdir::WalkDir;
use xxhash_rust::xxh3::Xxh3;

use voom_domain::errors::{Result, VoomError};
use voom_domain::events::FileDiscoveredEvent;

use crate::ScanOptions;

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
    let mut file = fs::File::open(path)?;
    let file_size = file.metadata()?.len();

    if file_size <= PARTIAL_HASH_THRESHOLD {
        return hash_file_full(&mut file);
    }

    let mut hasher = Xxh3::new();
    let mut buf = vec![0u8; HASH_CHUNK_SIZE];

    // Include file size in the hash so files with identical heads/tails
    // but different sizes produce different hashes.
    hasher.update(&file_size.to_le_bytes());

    // First chunk
    let n = file.read(&mut buf)?;
    hasher.update(&buf[..n]);

    // Middle chunk
    let mid = file_size / 2;
    file.seek(SeekFrom::Start(mid))?;
    let n = file.read(&mut buf)?;
    hasher.update(&buf[..n]);

    // Last chunk
    let tail_offset = file_size.saturating_sub(HASH_CHUNK_SIZE as u64);
    file.seek(SeekFrom::Start(tail_offset))?;
    let n = file.read(&mut buf)?;
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

    // Collect media file paths, reporting progress as we discover them
    let mut media_paths: Vec<_> = Vec::new();
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
            let path = entry.into_path();
            if let Some(ref cb) = options.on_progress {
                cb(crate::ScanProgress::Discovered {
                    count: media_paths.len() + 1,
                    path: path.clone(),
                });
            }
            media_paths.push(path);
        }
    }

    tracing::info!(
        root = %options.root.display(),
        count = media_paths.len(),
        "discovered media files"
    );

    let total = media_paths.len();
    let processed = Arc::new(AtomicUsize::new(0));

    // Process files in parallel using rayon
    let process_file = |path: &std::path::PathBuf| {
        let result = build_event(path, options.hash_files);
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

    // Collect results, logging errors but not failing the whole scan
    let mut discovered = Vec::with_capacity(events.len());
    for result in events {
        match result {
            Ok(event) => discovered.push(event),
            Err(e) => {
                tracing::warn!(error = %e, "failed to process file during discovery");
            }
        }
    }

    // Sort by path for deterministic output
    discovered.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(discovered)
}

/// Build a `FileDiscoveredEvent` for a single file.
fn build_event(path: &Path, compute_hash: bool) -> Result<FileDiscoveredEvent> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();

    let content_hash = if compute_hash {
        hash_file(path)?
    } else {
        String::new()
    };

    tracing::debug!(path = %path.display(), size, "file discovered");

    Ok(FileDiscoveredEvent {
        path: path.to_path_buf(),
        size,
        content_hash,
    })
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

        let event = build_event(&path, true).unwrap();
        assert_eq!(event.path, path);
        assert_eq!(event.size, 15);
        assert!(!event.content_hash.is_empty());
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
        };
        let events = scan_directory(&options).unwrap();
        // Should only find the file under "real/", not under "link/"
        assert_eq!(events.len(), 1);
        assert!(events[0].path.starts_with(&real_dir));
    }

    #[test]
    fn test_build_event_without_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mkv");
        std::fs::write(&path, b"fake video data").unwrap();

        let event = build_event(&path, false).unwrap();
        assert!(event.content_hash.is_empty());
    }
}
