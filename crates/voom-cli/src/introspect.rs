//! Shared introspection helpers used by both `scan` and `process` commands.
//!
//! # Event-driven introspection pipeline
//!
//! When `FileDiscovered` events are dispatched through the kernel:
//! 1. sqlite-store persists the discovered file in the staging table
//! 2. ffprobe-introspector enqueues a `JobType::Introspect` job
//!
//! The CLI drives introspection directly (not via the event bus) for
//! deterministic progress reporting and concurrency control, but the event
//! dispatch ensures all subscribers are notified.

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
    kernel.dispatch(Event::FileIntrospectionFailed(
        FileIntrospectionFailedEvent::new(
            path,
            size,
            content_hash,
            error.to_string(),
            BadFileSource::Introspection,
        ),
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
    kernel: &voom_kernel::Kernel,
    ffprobe_path: Option<&str>,
) -> std::result::Result<voom_domain::media::MediaFile, VoomError> {
    let mut introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    if let Some(fp) = ffprobe_path {
        introspector = introspector.with_ffprobe_path(fp);
    }
    let path_for_event = path.clone();
    let hash_for_event = content_hash.clone();
    let path_display = path.display().to_string();
    let hash_str = content_hash.clone().unwrap_or_default();
    let intro_result =
        tokio::task::spawn_blocking(move || introspector.introspect(&path, file_size, &hash_str))
            .await;

    match intro_result {
        Ok(Ok(intro_event)) => {
            let file = intro_event.file.clone();
            kernel.dispatch(Event::FileIntrospected(
                voom_domain::events::FileIntrospectedEvent::new(intro_event.file),
            ));
            Ok(file)
        }
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

/// Shared payload for jobs keyed on a discovered file (introspection, processing).
///
/// Used by both the ffprobe-introspector (enqueue) and CLI commands (dequeue).
#[derive(serde::Serialize, serde::Deserialize)]
pub struct DiscoveredFilePayload {
    pub path: String,
    pub size: u64,
    pub content_hash: Option<String>,
}
