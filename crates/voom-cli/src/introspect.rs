//! Shared introspection helpers used by both `scan` and `process` commands.
//!
//! # Introspection pipeline
//!
//! When `FileDiscovered` events are dispatched through the kernel,
//! sqlite-store persists the discovered file in the staging table.
//!
//! Introspection itself is driven directly (not via the event bus) for
//! deterministic progress reporting and concurrency control. The CLI calls
//! [`introspect_file`] for each file (or [`load_stored_file`] first, to
//! reuse the stored `MediaFile` when nothing has changed since the last
//! pass) and dispatches the resulting `FileIntrospected` event so
//! subscribers like sqlite-store can persist it.

use voom_domain::bad_file::BadFileSource;
use voom_domain::errors::VoomError;
use voom_domain::events::{Event, FileIntrospectionFailedEvent};

/// Dispatch a `FileIntrospectionFailed` event to the kernel.
pub fn dispatch_introspection_failure(
    kernel: &voom_kernel::Kernel,
    path: std::path::PathBuf,
    size: u64,
    content_hash: Option<String>,
    error: &str,
) {
    dispatch_failure(
        kernel,
        path,
        size,
        content_hash,
        error,
        BadFileSource::Introspection,
    );
}

/// Dispatch a file failure event with a specific [`BadFileSource`].
pub fn dispatch_failure(
    kernel: &voom_kernel::Kernel,
    path: std::path::PathBuf,
    size: u64,
    content_hash: Option<String>,
    error: &str,
    source: BadFileSource,
) {
    kernel.dispatch(Event::FileIntrospectionFailed(
        FileIntrospectionFailedEvent::new(path, size, content_hash, error.to_string(), source),
    ));
}

/// Run ffprobe introspection on a single file (blocking I/O on a `spawn_blocking` thread).
///
/// Creates a standalone `FfprobeIntrospectorPlugin` per call rather than using
/// the kernel-registered instance. This is necessary because each call runs on
/// a separate `spawn_blocking` thread and the plugin is not `Clone`.
/// Pass `ffprobe_path` to use a custom ffprobe binary (e.g. from config).
/// Events are dispatched to the kernel for downstream subscribers.
pub async fn introspect_file(
    path: std::path::PathBuf,
    file_size: u64,
    content_hash: Option<String>,
    kernel: &std::sync::Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<&str>,
) -> std::result::Result<voom_domain::media::MediaFile, VoomError> {
    introspect_file_inner(path, file_size, content_hash, kernel, ffprobe_path, true).await
}

/// Like [`introspect_file`] but skips the `FileIntrospected` event dispatch.
/// Use this from code paths that take responsibility for persistence
/// themselves (e.g. the post-execution bundled write).
// Called by process::handle_plan_success (Task 5). The binary-crate module
// boundary means rustc sees this as dead until that call-site exists.
#[allow(dead_code)]
pub async fn introspect_file_no_dispatch(
    path: std::path::PathBuf,
    file_size: u64,
    content_hash: Option<String>,
    kernel: &std::sync::Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<&str>,
) -> std::result::Result<voom_domain::media::MediaFile, VoomError> {
    introspect_file_inner(path, file_size, content_hash, kernel, ffprobe_path, false).await
}

async fn introspect_file_inner(
    path: std::path::PathBuf,
    file_size: u64,
    content_hash: Option<String>,
    kernel: &std::sync::Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<&str>,
    dispatch_event: bool,
) -> std::result::Result<voom_domain::media::MediaFile, VoomError> {
    let mut introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    if let Some(fp) = ffprobe_path {
        introspector = introspector.with_ffprobe_path(fp);
    }
    let path_for_event = path.clone();
    let hash_for_event = content_hash.clone();
    let path_display = path.display().to_string();
    // Run introspection AND (optionally) the FileIntrospected dispatch
    // inside spawn_blocking so the entire cascade (WASM plugins,
    // subtitle mux subscribers) runs on the blocking pool rather than
    // the tokio runtime.
    let kernel_clone = kernel.clone();
    let intro_result = tokio::task::spawn_blocking(move || {
        let result = introspector.introspect(&path, file_size, content_hash.as_deref());
        if dispatch_event {
            if let Ok(ref intro_event) = result {
                kernel_clone.dispatch(Event::FileIntrospected(
                    voom_domain::events::FileIntrospectedEvent::new(intro_event.file.clone()),
                ));
            }
        }
        result
    })
    .await;

    match intro_result {
        Ok(Ok(intro_event)) => Ok(intro_event.file),
        Err(join_err) => {
            let error_msg = format!("task join error for {path_display}: {join_err}");
            dispatch_introspection_failure(
                kernel,
                path_for_event,
                file_size,
                hash_for_event,
                &error_msg,
            );
            Err(VoomError::plugin("ffprobe", &error_msg))
        }
        Ok(Err(e)) => {
            dispatch_introspection_failure(
                kernel,
                path_for_event,
                file_size,
                hash_for_event,
                &e.to_string(),
            );
            Err(VoomError::plugin(
                "ffprobe",
                format!("introspection failed for {path_display}: {e}"),
            ))
        }
    }
}

pub use voom_domain::DiscoveredFilePayload;

/// Load the stored [`MediaFile`](voom_domain::media::MediaFile) for `path`,
/// or `None` if there is no row, the lookup failed, or the runtime panicked.
///
/// Runs the synchronous `file_by_path` lookup on `spawn_blocking` because
/// `StorageTrait` is built on `rusqlite`. Storage and join failures are logged
/// and surfaced as `None` so callers can fall back gracefully.
pub async fn load_stored_file(
    store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
    path: std::path::PathBuf,
) -> Option<voom_domain::media::MediaFile> {
    let lookup_path = path.clone();
    tokio::task::spawn_blocking(move || store.file_by_path(&lookup_path))
        .await
        .map_err(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "stored file lookup join failed"
            );
        })
        .ok()?
        .map_err(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "stored file lookup failed"
            );
        })
        .ok()
        .flatten()
}

/// Return `true` when `stored` can stand in for a fresh ffprobe pass.
///
/// All of these must hold:
///
/// - `stored.status == FileStatus::Active`.
/// - `stored.size == discovered_size`.
/// - `discovered_hash` is `Some` AND equals `stored.content_hash` (the
///   `--no-backup` path produces no hash, so we cannot prove the file is
///   unchanged and must re-introspect).
/// - `stored.tracks` is non-empty (guards against partially-populated rows
///   from a previous failed introspection).
#[must_use]
pub fn matches_discovery(
    stored: &voom_domain::media::MediaFile,
    discovered_size: u64,
    discovered_hash: Option<&str>,
) -> bool {
    let hash_match = matches!(
        (stored.content_hash.as_deref(), discovered_hash),
        (Some(s), Some(d)) if s == d
    );
    stored.status == voom_domain::transition::FileStatus::Active
        && stored.size == discovered_size
        && hash_match
        && !stored.tracks.is_empty()
}

/// Build a fingerprint lookup closure backed by the given storage.
///
/// The closure delegates to `StorageTrait::file_fingerprint_by_path`, which
/// the sqlite-store backend serves from a narrow query. Storage errors are
/// logged at warn level and the closure returns `None` so discovery falls
/// back to hashing — a sustained storage failure would otherwise cause a
/// library-wide silent re-hash with no operator visibility.
#[must_use]
pub fn fingerprint_lookup(
    store: std::sync::Arc<dyn voom_domain::storage::StorageTrait>,
) -> voom_discovery::FingerprintLookup {
    Box::new(move |path| match store.file_fingerprint_by_path(path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "fingerprint lookup failed; falling back to rehash"
            );
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::storage::StorageTrait;

    fn store() -> Arc<dyn StorageTrait> {
        Arc::new(voom_sqlite_store::store::SqliteStore::in_memory().expect("in-memory store"))
    }

    fn introspected_file(path: &str, size: u64, hash: Option<&str>) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path))
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())]);
        file.size = size;
        file.content_hash = hash.map(str::to_string);
        file.expected_hash = hash.map(str::to_string);
        file
    }

    #[tokio::test]
    async fn load_stored_file_returns_row() {
        let store = store();
        store
            .upsert_file(&introspected_file("/media/x.mkv", 1024, Some("abc")))
            .unwrap();

        let loaded = load_stored_file(store, PathBuf::from("/media/x.mkv"))
            .await
            .expect("row should exist");
        assert_eq!(loaded.size, 1024);
    }

    #[tokio::test]
    async fn load_stored_file_returns_none_for_unknown_path() {
        assert!(
            load_stored_file(store(), PathBuf::from("/media/missing.mkv"))
                .await
                .is_none()
        );
    }

    #[test]
    fn matches_discovery_accepts_exact_match() {
        let stored = introspected_file("/media/x.mkv", 1024, Some("abc"));
        assert!(matches_discovery(&stored, 1024, Some("abc")));
    }

    #[test]
    fn matches_discovery_rejects_size_mismatch() {
        let stored = introspected_file("/media/x.mkv", 1024, Some("abc"));
        assert!(!matches_discovery(&stored, 2048, Some("abc")));
    }

    #[test]
    fn matches_discovery_rejects_hash_mismatch() {
        let stored = introspected_file("/media/x.mkv", 1024, Some("abc"));
        assert!(!matches_discovery(&stored, 1024, Some("def")));
    }

    #[test]
    fn matches_discovery_rejects_when_no_discovered_hash() {
        let stored = introspected_file("/media/x.mkv", 1024, Some("abc"));
        assert!(!matches_discovery(&stored, 1024, None));
    }

    #[test]
    fn matches_discovery_rejects_when_stored_hash_absent() {
        let stored = introspected_file("/media/x.mkv", 1024, None);
        assert!(!matches_discovery(&stored, 1024, Some("abc")));
    }

    #[test]
    fn matches_discovery_rejects_missing_status() {
        let mut stored = introspected_file("/media/x.mkv", 1024, Some("abc"));
        stored.status = voom_domain::transition::FileStatus::Missing;
        assert!(!matches_discovery(&stored, 1024, Some("abc")));
    }

    #[test]
    fn matches_discovery_rejects_empty_tracks() {
        let mut stored = MediaFile::new(PathBuf::from("/media/x.mkv"));
        stored.size = 1024;
        stored.content_hash = Some("abc".into());
        assert!(!matches_discovery(&stored, 1024, Some("abc")));
    }
}
