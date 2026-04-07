//! Integration test for the `/events` SSE endpoint.
//!
//! Regression test for issue #135: events must reach browsers with named
//! SSE event types so frontend `addEventListener('job-update', ...)` etc.
//! actually fire. Before the fix, every event was emitted with the default
//! `message` name and no frontend listener triggered.
//!
//! The test binds the real router to a loopback TCP listener, issues a raw
//! HTTP/1.1 GET to `/events`, broadcasts one event of each category over the
//! shared `sse_tx`, then reads bytes from the socket and asserts the SSE frame
//! format contains the expected `event: <name>` line.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::time::timeout;

use voom_domain::test_support::InMemoryStore;
use voom_web_server::state::{AppState, SseEvent, SSE_CHANNEL_CAPACITY};

/// Read from the socket until the HTTP response header block (`\r\n\r\n`) has
/// been consumed. Returns any bytes already read past the header boundary so
/// the caller can continue parsing the body.
async fn read_past_headers(stream: &mut TcpStream) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    loop {
        let n = timeout(Duration::from_secs(2), stream.read(&mut chunk))
            .await
            .expect("timed out waiting for HTTP response headers")
            .expect("socket read failed during header read");
        assert!(n > 0, "server closed connection before sending headers");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(idx) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            return buf[idx + 4..].to_vec();
        }
    }
}

/// Read bytes from the socket until `needle` appears in the accumulated body
/// or the timeout elapses. Returns the accumulated body bytes including the
/// match. Fails the test on timeout with a diagnostic dump.
async fn read_until(stream: &mut TcpStream, mut body: Vec<u8>, needle: &str) -> Vec<u8> {
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if find_subslice(&body, needle.as_bytes()).is_some() {
            return body;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "timed out waiting for {needle:?} in SSE body. Accumulated body:\n{}",
                String::from_utf8_lossy(&body)
            );
        }
        let n = match timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => panic!("socket read error: {e}"),
            Err(_) => panic!(
                "timed out waiting for {needle:?} in SSE body. Accumulated body:\n{}",
                String::from_utf8_lossy(&body)
            ),
        };
        if n == 0 {
            panic!(
                "server closed connection before emitting {needle:?}. Accumulated body:\n{}",
                String::from_utf8_lossy(&body)
            );
        }
        body.extend_from_slice(&chunk[..n]);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Spawn the real web-server router bound to a loopback port and open a raw
/// TCP SSE connection to `/events`. Returns the connected stream (with the
/// HTTP response headers already consumed) along with the shared `sse_tx` so
/// the test can broadcast events that the handler will forward.
async fn spawn_server_and_connect() -> (TcpStream, broadcast::Sender<SseEvent>) {
    let (sse_tx, _) = broadcast::channel(SSE_CHANNEL_CAPACITY);
    let store = Arc::new(InMemoryStore::new());
    let templates = voom_web_server::server::embedded_templates().expect("embedded templates");
    let state = AppState::new(store, sse_tx.clone(), templates, None, None);
    let router = voom_web_server::router::build_router(state);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        // Ignore the result: when the test runtime is dropped, this future is
        // cancelled mid-await, which is the expected lifecycle.
        let _ = axum::serve(listener, router).await;
    });

    let mut stream = TcpStream::connect(addr).await.expect("connect loopback");
    stream
        .write_all(
            b"GET /events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write request");

    let leftover = read_past_headers(&mut stream).await;
    // Any bytes leftover after the header block (unlikely but possible with
    // keep-alive comments) are discarded here — tests below only look for
    // named event lines, which will arrive after the broadcast.
    drop(leftover);

    // Wait deterministically for the SSE handler to register its broadcast
    // subscription. `events_handler` calls `sse_tx.subscribe()` synchronously
    // before returning, so the response headers (already consumed above) are
    // strong evidence the receiver exists — but the BroadcastStream's first
    // poll happens lazily on the body-writing task. Polling `receiver_count`
    // is faster and more deterministic than sleeping a fixed duration.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while sse_tx.receiver_count() == 0 {
        if tokio::time::Instant::now() >= deadline {
            panic!("SSE handler never subscribed to broadcast channel");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    (stream, sse_tx)
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_job_event_has_job_update_name() {
    let (mut stream, sse_tx) = spawn_server_and_connect().await;

    sse_tx
        .send(SseEvent::JobStarted {
            job_id: "job-1".into(),
            description: "test".into(),
        })
        .expect("send job event");

    let body = read_until(&mut stream, Vec::new(), "event: job-update").await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("event: job-update"),
        "expected 'event: job-update' in body, got:\n{text}"
    );
    assert!(
        text.contains("\"JobStarted\""),
        "expected JobStarted payload in body, got:\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_file_event_has_file_update_name() {
    let (mut stream, sse_tx) = spawn_server_and_connect().await;

    sse_tx
        .send(SseEvent::FileIntrospected {
            path: "/media/test.mkv".into(),
        })
        .expect("send file event");

    let body = read_until(&mut stream, Vec::new(), "event: file-update").await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("event: file-update"),
        "expected 'event: file-update' in body, got:\n{text}"
    );
    assert!(
        text.contains("FileIntrospected"),
        "expected FileIntrospected payload in body, got:\n{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_plan_event_has_plan_update_name() {
    let (mut stream, sse_tx) = spawn_server_and_connect().await;

    sse_tx
        .send(SseEvent::PlanExecuting {
            plan_id: "plan-1".into(),
            file: "movie.mkv".into(),
            phase: "clean".into(),
            action_count: 3,
        })
        .expect("send plan event");

    let body = read_until(&mut stream, Vec::new(), "event: plan-update").await;
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("event: plan-update"),
        "expected 'event: plan-update' in body, got:\n{text}"
    );
    assert!(
        text.contains("PlanExecuting"),
        "expected PlanExecuting payload in body, got:\n{text}"
    );
}
