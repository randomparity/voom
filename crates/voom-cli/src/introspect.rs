//! Shared introspection helpers used by both `scan` and `process` commands.
//!
//! # Event-driven introspection pipeline
//!
//! When `FileDiscovered` events are dispatched through the kernel:
//! 1. sqlite-store persists the discovered file in the staging table
//! 2. ffprobe-introspector enqueues a `JobType::Introspect` job
//! 3. The CLI processes these jobs via `process_introspection_job`
//!
//! The CLI still drives introspection directly (not via the event bus) for
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
    content_hash: String,
    error: &str,
) {
    kernel.dispatch(Event::FileIntrospectionFailed(
        FileIntrospectionFailedEvent::new(
            path,
            size,
            Some(content_hash),
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
    content_hash: String,
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
    let intro_result = tokio::task::spawn_blocking(move || {
        introspector.introspect(&path, file_size, &content_hash)
    })
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
    pub content_hash: String,
}

/// Process a single introspection job from the job queue.
///
/// Deserializes the job payload, runs ffprobe via `introspect_file`, and
/// returns the result for the worker pool to mark as completed or failed.
#[allow(dead_code)] // will be called from daemon mode (#36)
pub async fn process_introspection_job(
    job: &voom_domain::job::Job,
    kernel: &voom_kernel::Kernel,
    ffprobe_path: Option<&str>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let raw_payload = job
        .payload
        .as_ref()
        .ok_or_else(|| "missing introspection job payload".to_string())?;

    let payload: DiscoveredFilePayload = serde_json::from_value(raw_payload.clone())
        .map_err(|e| format!("invalid introspection payload: {e}"))?;

    let path = std::path::PathBuf::from(&payload.path);

    let file = introspect_file(
        path,
        payload.size,
        payload.content_hash,
        kernel,
        ffprobe_path,
    )
    .await
    .map_err(|e| format!("introspect {}: {e}", payload.path))?;

    Ok(Some(serde_json::json!({
        "path": file.path.display().to_string(),
        "tracks": file.tracks.len(),
        "container": format!("{:?}", file.container),
    })))
}
