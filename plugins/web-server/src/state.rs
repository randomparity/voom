//! Shared application state for the web server.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use tokio::sync::broadcast;
use voom_domain::storage::StorageTrait;

/// Events broadcast via SSE to connected clients.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", content = "data")]
pub enum SseEvent {
    JobStarted {
        job_id: String,
        description: String,
    },
    JobProgress {
        job_id: String,
        progress: f64,
        message: Option<String>,
    },
    JobCompleted {
        job_id: String,
        success: bool,
        message: Option<String>,
    },
    ScanProgress {
        files_found: u64,
        files_processed: u64,
    },
    FileIntrospected {
        path: String,
    },
}

/// Application state shared across all handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn StorageTrait>,
    pub sse_tx: broadcast::Sender<SseEvent>,
    pub templates: Arc<tera::Tera>,
    pub auth_token: Option<String>,
    pub sse_client_count: Arc<AtomicU32>,
}

impl AppState {
    pub fn new(
        store: Arc<dyn StorageTrait>,
        templates: tera::Tera,
        auth_token: Option<String>,
    ) -> Self {
        let (sse_tx, _) = broadcast::channel(256);
        Self {
            store,
            sse_tx,
            templates: Arc::new(templates),
            auth_token,
            sse_client_count: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Validate an Authorization header value against the configured auth token.
    /// Returns true if no auth is configured (allow all) or if the Bearer token matches.
    pub fn validate_auth(&self, header: Option<&str>) -> bool {
        match &self.auth_token {
            None => true, // no auth configured, allow all
            Some(token) => {
                header.is_some_and(|h| h.strip_prefix("Bearer ").is_some_and(|t| t == token))
            }
        }
    }
}
