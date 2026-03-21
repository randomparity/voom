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

impl SseEvent {
    /// Try to convert a domain Event into an `SseEvent`.
    /// Returns None for event types that don't have SSE representations.
    #[must_use]
    pub fn from_domain(event: &voom_domain::events::Event) -> Option<Self> {
        use voom_domain::events::Event;
        match event {
            Event::JobStarted(e) => Some(SseEvent::JobStarted {
                job_id: e.job_id.to_string(),
                description: e.description.clone(),
            }),
            Event::JobProgress(e) => Some(SseEvent::JobProgress {
                job_id: e.job_id.to_string(),
                progress: e.progress,
                message: e.message.clone(),
            }),
            Event::JobCompleted(e) => Some(SseEvent::JobCompleted {
                job_id: e.job_id.to_string(),
                success: e.success,
                message: e.message.clone(),
            }),
            Event::FileIntrospected(e) => Some(SseEvent::FileIntrospected {
                path: e.file.path.display().to_string(),
            }),
            _ => None,
        }
    }
}

/// Application state shared across all handlers.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn StorageTrait>,
    pub sse_tx: broadcast::Sender<SseEvent>,
    pub templates: Arc<tera::Tera>,
    pub auth_token: Option<String>,
    pub sse_client_count: Arc<AtomicU32>,
    pub plugin_info: Arc<Vec<crate::api::plugins::PluginInfo>>,
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
            plugin_info: Arc::new(Vec::new()),
        }
    }

    /// Set the plugin info snapshot (typically populated from kernel registry at startup).
    #[must_use]
    pub fn with_plugin_info(mut self, info: Vec<crate::api::plugins::PluginInfo>) -> Self {
        self.plugin_info = Arc::new(info);
        self
    }

    /// Validate an Authorization header value against the configured auth token.
    /// Returns true if no auth is configured (allow all) or if the Bearer token matches.
    /// Uses constant-time comparison to prevent timing side-channel attacks.
    #[must_use]
    pub fn is_authorized(&self, header: Option<&str>) -> bool {
        use subtle::ConstantTimeEq;
        match &self.auth_token {
            None => true, // no auth configured, allow all
            Some(token) => header.is_some_and(|h| {
                h.strip_prefix("Bearer ").is_some_and(|t| {
                    t.len() == token.len() && bool::from(t.as_bytes().ct_eq(token.as_bytes()))
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::test_support::InMemoryStore;

    fn make_state(auth_token: Option<String>) -> AppState {
        let store = Arc::new(InMemoryStore::new());
        let templates = tera::Tera::default();
        AppState::new(store, templates, auth_token)
    }

    #[test]
    fn new_creates_state_with_broadcast_channel() {
        let state = make_state(None);
        // sse_tx should be usable: subscribing should succeed
        let _rx = state.sse_tx.subscribe();
        assert!(state.auth_token.is_none());
    }

    #[test]
    fn new_with_auth_token() {
        let state = make_state(Some("my-secret".into()));
        assert_eq!(state.auth_token, Some("my-secret".to_string()));
    }

    #[test]
    fn sse_client_count_starts_at_zero() {
        let state = make_state(None);
        assert_eq!(
            state
                .sse_client_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn is_authorized_no_token_configured_allows_all() {
        let state = make_state(None);
        assert!(state.is_authorized(None));
        assert!(state.is_authorized(Some("Bearer anything")));
        assert!(state.is_authorized(Some("garbage")));
    }

    #[test]
    fn is_authorized_with_token_requires_bearer_prefix() {
        let state = make_state(Some("secret123".into()));
        assert!(state.is_authorized(Some("Bearer secret123")));
        assert!(!state.is_authorized(Some("secret123")));
        assert!(!state.is_authorized(Some("bearer secret123")));
        assert!(!state.is_authorized(Some("Token secret123")));
    }

    #[test]
    fn is_authorized_with_token_rejects_none() {
        let state = make_state(Some("secret123".into()));
        assert!(!state.is_authorized(None));
    }

    #[test]
    fn is_authorized_with_token_rejects_wrong_token() {
        let state = make_state(Some("secret123".into()));
        assert!(!state.is_authorized(Some("Bearer wrong")));
        assert!(!state.is_authorized(Some("Bearer secret1234")));
        assert!(!state.is_authorized(Some("Bearer ")));
    }

    #[test]
    fn state_is_clone() {
        let state = make_state(Some("tok".into()));
        let cloned = state.clone();
        assert_eq!(cloned.auth_token, Some("tok".to_string()));
        // Cloned state shares the same Arc references
        assert!(Arc::ptr_eq(&state.templates, &cloned.templates));
        assert!(Arc::ptr_eq(
            &state.sse_client_count,
            &cloned.sse_client_count
        ));
    }

    #[test]
    fn sse_broadcast_delivers_events() {
        let state = make_state(None);
        let mut rx = state.sse_tx.subscribe();
        let event = SseEvent::FileIntrospected {
            path: "/media/test.mkv".into(),
        };
        state.sse_tx.send(event.clone()).unwrap();
        let received = rx.try_recv().unwrap();
        match received {
            SseEvent::FileIntrospected { path } => assert_eq!(path, "/media/test.mkv"),
            _ => panic!("unexpected event variant"),
        }
    }

    #[test]
    fn sse_event_serialization_tagged() {
        let event = SseEvent::JobStarted {
            job_id: "j1".into(),
            description: "test job".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "JobStarted");
        assert_eq!(json["data"]["job_id"], "j1");
        assert_eq!(json["data"]["description"], "test job");
    }

    #[test]
    fn sse_event_scan_progress_serialization() {
        let event = SseEvent::ScanProgress {
            files_found: 42,
            files_processed: 10,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ScanProgress");
        assert_eq!(json["data"]["files_found"], 42);
        assert_eq!(json["data"]["files_processed"], 10);
    }

    #[test]
    fn sse_event_job_completed_serialization() {
        let event = SseEvent::JobCompleted {
            job_id: "j2".into(),
            success: true,
            message: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "JobCompleted");
        assert_eq!(json["data"]["success"], true);
        assert_eq!(json["data"]["message"], serde_json::Value::Null);
    }
}
