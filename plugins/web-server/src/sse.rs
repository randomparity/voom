//! Server-Sent Events (SSE) for live updates.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event as SseAxumEvent, KeepAlive, Sse};
use futures_core::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::state::AppState;

/// SSE endpoint: streams live events to connected clients.
pub async fn events_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseAxumEvent, Infallible>>> {
    let rx = state.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let json = serde_json::to_string(&event).ok()?;
            Some(Ok(SseAxumEvent::default().data(json)))
        }
        Err(_) => None, // Lagged — skip missed events
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
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
