//! Shared application state for the web server.

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
}

impl AppState {
    pub fn new(store: Arc<dyn StorageTrait>, templates: tera::Tera) -> Self {
        let (sse_tx, _) = broadcast::channel(256);
        Self {
            store,
            sse_tx,
            templates: Arc::new(templates),
        }
    }
}
