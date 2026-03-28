//! Shared introspection helpers used by both `scan` and `process` commands.
//!
//! # Dual-dispatch pattern (direct-call + event-publish)
//!
//! CLI commands call plugin APIs directly (e.g. `FfprobeIntrospectorPlugin::introspect`)
//! rather than dispatching a `FileDiscovered` event and letting the kernel route it to
//! the introspector. Direct calls give the CLI deterministic control over ordering,
//! concurrency, and progress reporting — things the fire-and-forget event bus cannot
//! provide.
//!
//! After the direct call completes, the result is published as a `FileIntrospected`
//! event through the kernel so that downstream subscribers (sqlite-store, SSE, WASM
//! plugins) still receive it. This is the "direct-call + event-publish" pattern used
//! throughout the CLI layer.
//!
//! A fresh `FfprobeIntrospectorPlugin` is created per call because each invocation
//! runs on a separate `spawn_blocking` thread and the plugin type is not `Clone`.

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
