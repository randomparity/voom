//! Filesystem discovery plugin: parallel directory walking with content hashing.

pub mod scanner;
#[cfg(feature = "test-hooks")]
pub mod test_hooks;

pub use scanner::EventSink;
pub use scanner::hash_file;
pub use scanner::normalize_path;
pub use scanner::scan_directory_streaming;

use voom_domain::call::{Call, CallResponse, ScanSummary};
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{FileDiscoveredEvent, RootWalkCompletedEvent};
pub use voom_domain::scan::{
    ErrorCallback, FingerprintLookup, MutationSnapshotLoader, ScanOptions, ScanProgress,
};
pub use voom_domain::scan_session_mutations::SessionMutationSnapshot;
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

    /// Streaming scan. See [`scanner::scan_directory_streaming`].
    pub fn scan_streaming(
        &self,
        options: &ScanOptions,
        on_event: scanner::EventSink,
    ) -> Result<()> {
        scanner::scan_directory_streaming(options, on_event)
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

    fn on_call(&self, call: &Call) -> voom_domain::errors::Result<CallResponse> {
        let Call::ScanLibrary {
            options,
            scan_session,
            sink,
            root_done,
            cancel,
            uri: _,
        } = call
        else {
            return Err(voom_domain::errors::VoomError::plugin(
                self.name(),
                format!(
                    "DiscoveryPlugin only handles Call::ScanLibrary, got {:?}",
                    std::mem::discriminant(call)
                ),
            ));
        };

        // Shared counter — populated from inside the rayon worker callback.
        let file_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

        let count_clone = file_count.clone();
        let sink_clone = sink.clone();
        let cancel_clone = cancel.clone();

        let started = std::time::Instant::now();

        // The scanner is synchronous and rayon-parallel. Forward each
        // FileDiscoveredEvent into the caller's tokio mpsc via `blocking_send`.
        // `blocking_send` applies natural backpressure: when the receiver is
        // full, the rayon worker blocks until drain. A `SendError` here means
        // the receiver was dropped — caller went away, safe to swallow; the
        // next `cancel_clone.is_cancelled()` check will short-circuit any
        // remaining work.
        let on_event: scanner::EventSink = Box::new(move |event| {
            if cancel_clone.is_cancelled() {
                return;
            }
            match sink_clone.blocking_send(event) {
                Ok(()) => {
                    count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(_send_error) => {
                    // Receiver dropped — caller no longer cares. Stop counting,
                    // let the scanner finish naturally; the next cancel check
                    // will short-circuit remaining work.
                    tracing::debug!(
                        "scan sink receiver dropped; remaining events will be discarded"
                    );
                }
            }
        });

        let scan_result = scanner::scan_directory_streaming(options, on_event);

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        match scan_result {
            Ok(()) => {
                // Send the root-walk-completed signal *only* after a successful
                // scan, mirroring the Call::ScanLibrary doc contract.
                if let Some(done) = root_done {
                    if let Err(e) = done.try_send(RootWalkCompletedEvent::new(
                        options.root.clone(),
                        *scan_session,
                        duration_ms,
                    )) {
                        tracing::warn!(
                            error = %e,
                            root = %options.root.display(),
                            "failed to deliver RootWalkCompletedEvent",
                        );
                    }
                }
                let summary = ScanSummary::new(
                    file_count.load(std::sync::atomic::Ordering::Relaxed),
                    vec![],
                    1,
                );
                Ok(CallResponse::ScanLibrary(summary))
            }
            Err(e) => Err(e),
        }
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
    fn test_scan_reuses_cached_hash_when_fingerprint_matches() {
        use chrono::Utc;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();

        let canonical = normalize_path(&file_path);
        let file_size = std::fs::metadata(&file_path).unwrap().len();
        let cached_hash = "cached-hash-sentinel".to_string();

        // Fingerprint "last seen" is in the future so the on-disk mtime is
        // guaranteed to be earlier.
        let stored = StoredFingerprint {
            size: file_size,
            content_hash: cached_hash.clone(),
            last_seen: Utc::now() + chrono::Duration::hours(1),
        };

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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();

        let stored = StoredFingerprint {
            size: 9_999_999, // deliberately wrong
            content_hash: "stale-hash".to_string(),
            last_seen: Utc::now() + chrono::Duration::hours(1),
        };

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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use voom_domain::media::StoredFingerprint;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mkv");
        std::fs::write(&file_path, b"fake mkv data").unwrap();
        let file_size = std::fs::metadata(&file_path).unwrap().len();

        let stored = StoredFingerprint {
            size: file_size,
            content_hash: "stale-hash".to_string(),
            last_seen: Utc::now() - chrono::Duration::days(365),
        };

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
    fn on_call_scan_library_emits_events_to_sink() {
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;
        use voom_domain::transition::ScanSessionId;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("video.mkv"), b"fake mkv data").unwrap();
        std::fs::write(dir.path().join("audio.mp4"), b"fake mp4 data").unwrap();
        std::fs::write(dir.path().join("readme.txt"), b"not media").unwrap();

        let (sink, mut rx) = mpsc::channel::<FileDiscoveredEvent>(16);
        let (root_done_tx, mut root_done_rx) = mpsc::channel::<RootWalkCompletedEvent>(1);
        let cancel = CancellationToken::new();
        let scan_session = ScanSessionId::new();

        let call = Call::ScanLibrary {
            uri: format!("file://{}", dir.path().display()),
            options: ScanOptions::new(dir.path()),
            scan_session,
            sink,
            root_done: Some(root_done_tx),
            cancel,
        };

        let plugin = DiscoveryPlugin::new();
        let response = plugin.on_call(&call).expect("on_call should succeed");

        // Drain the events synchronously: on_call returned, so the rayon
        // workers have all finished pushing into the channel.
        let mut received = Vec::new();
        while let Ok(event) = rx.try_recv() {
            received.push(event);
        }
        assert_eq!(received.len(), 2, "should receive 2 media events");

        // root_done must be emitted exactly once, carrying the caller's session id.
        let root_done_event = root_done_rx
            .try_recv()
            .expect("RootWalkCompletedEvent must be emitted on success");
        assert_eq!(root_done_event.session, scan_session);
        assert!(
            root_done_rx.try_recv().is_err(),
            "exactly one RootWalkCompletedEvent expected"
        );

        let CallResponse::ScanLibrary(summary) = response else {
            panic!("expected CallResponse::ScanLibrary");
        };
        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.roots_scanned, 1);
        assert!(summary.errors.is_empty(), "errors should be empty");
    }

    #[test]
    fn on_call_does_not_emit_root_done_when_scan_fails() {
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;
        use voom_domain::transition::ScanSessionId;

        let (sink, _rx) = mpsc::channel::<FileDiscoveredEvent>(16);
        let (root_done_tx, mut root_done_rx) = mpsc::channel::<RootWalkCompletedEvent>(1);
        let cancel = CancellationToken::new();
        let scan_session = ScanSessionId::new();

        let call = Call::ScanLibrary {
            uri: "file:///definitely/does/not/exist/scratch/scan".into(),
            options: ScanOptions::new("/definitely/does/not/exist/scratch/scan"),
            scan_session,
            sink,
            root_done: Some(root_done_tx),
            cancel,
        };

        let plugin = DiscoveryPlugin::new();
        let result = plugin.on_call(&call);
        assert!(result.is_err(), "scanning a missing path must return Err");
        assert!(
            root_done_rx.try_recv().is_err(),
            "RootWalkCompletedEvent must NOT be emitted on scan failure"
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
