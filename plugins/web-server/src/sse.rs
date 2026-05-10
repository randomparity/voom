//! Server-Sent Events (SSE) for live updates.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event as SseAxumEvent, KeepAlive, Sse};
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::state::{AppState, SseEvent};

/// Map an `SseEvent` variant to the SSE event name clients listen for.
///
/// The frontend in `base.html` registers listeners by these names, so changing
/// a name here must be matched by a corresponding update in the template. The
/// match is intentionally exhaustive to force a compile error when new
/// variants are added.
fn sse_event_name(event: &SseEvent) -> &'static str {
    match event {
        SseEvent::JobStarted { .. }
        | SseEvent::JobProgress { .. }
        | SseEvent::JobCompleted { .. } => "job-update",
        SseEvent::FileIntrospected { .. } => "file-update",
        SseEvent::PlanExecuting { .. }
        | SseEvent::PlanCompleted { .. }
        | SseEvent::PlanSkipped { .. }
        | SseEvent::PlanFailed { .. } => "plan-update",
    }
}

/// Maximum number of concurrent SSE clients.
const MAX_SSE_CLIENTS: u32 = 64;

/// RAII guard that decrements the SSE client counter when dropped.
struct SseClientGuard {
    counter: Arc<AtomicU32>,
}

impl Drop for SseClientGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// SSE endpoint: streams live events to connected clients.
pub async fn events_handler(
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<SseAxumEvent, Infallible>>>, impl IntoResponse> {
    // Enforce maximum concurrent SSE client limit.
    let current = state.sse_client_count.fetch_add(1, Ordering::Relaxed);
    if current >= MAX_SSE_CLIENTS {
        state.sse_client_count.fetch_sub(1, Ordering::Relaxed);
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"error": "Too many SSE clients"})),
        ));
    }

    let guard = SseClientGuard {
        counter: state.sse_client_count.clone(),
    };

    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        let _guard = &guard; // keep guard alive for the lifetime of the stream
        match result {
            Ok(event) => {
                let name = sse_event_name(&event);
                match serde_json::to_string(&event) {
                    Ok(json) => Some(Ok(SseAxumEvent::default().event(name).data(json))),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize SSE event");
                        None
                    }
                }
            }
            Err(BroadcastStreamRecvError::Lagged(count)) => {
                let json = serde_json::json!({"type": "lagged", "missed": count}).to_string();
                Some(Ok(SseAxumEvent::default().event("lagged").data(json)))
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use crate::state::SseEvent;

    #[test]
    fn test_sse_event_serialization() {
        let event = SseEvent::JobProgress {
            job_id: "123".into(),
            progress: 0.5,
            message: Some("halfway".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("JobProgress"));
        assert!(json.contains("0.5"));
    }
}
