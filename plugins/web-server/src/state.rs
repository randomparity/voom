//! Shared application state for the web server.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use tokio::sync::broadcast;
use voom_domain::storage::StorageTrait;

/// Capacity of the SSE broadcast channel. Sized to absorb short bursts of
/// job-progress events without lagging slow clients.
pub const SSE_CHANNEL_CAPACITY: usize = 256;

/// Request handed from the Web API to the application-level process launcher.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProcessRunLaunchRequest {
    pub paths: Vec<std::path::PathBuf>,
    pub policy: Option<std::path::PathBuf>,
    pub policy_map: Option<std::path::PathBuf>,
    pub workers: usize,
    pub force_rescan: bool,
    pub estimate_id: uuid::Uuid,
}

impl ProcessRunLaunchRequest {
    #[must_use]
    pub fn new(paths: Vec<std::path::PathBuf>, estimate_id: uuid::Uuid) -> Self {
        Self {
            paths,
            policy: None,
            policy_map: None,
            workers: 0,
            force_rescan: false,
            estimate_id,
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: Option<std::path::PathBuf>) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn with_policy_map(mut self, policy_map: Option<std::path::PathBuf>) -> Self {
        self.policy_map = policy_map;
        self
    }

    #[must_use]
    pub fn with_workers(mut self, workers: usize) -> Self {
        self.workers = workers;
        self
    }

    #[must_use]
    pub fn with_force_rescan(mut self, force_rescan: bool) -> Self {
        self.force_rescan = force_rescan;
        self
    }
}

/// Response returned by the application-level process launcher.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct ProcessRunLaunchResponse {
    pub run_id: String,
    pub message: String,
}

impl ProcessRunLaunchResponse {
    #[must_use]
    pub fn new(run_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            message: message.into(),
        }
    }
}

/// Application-level process launcher injected by the CLI binary.
pub trait ProcessRunLauncher: Send + Sync {
    /// Launch a confirmed process run.
    ///
    /// # Errors
    /// Returns a user-facing message when the launch request cannot be queued
    /// or started.
    fn launch(&self, request: ProcessRunLaunchRequest) -> Result<ProcessRunLaunchResponse, String>;
}

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
    FileIntrospected {
        path: String,
    },
    PlanExecuting {
        plan_id: String,
        /// Basename of the media file the plan is being applied to.
        file: String,
        phase: String,
        action_count: usize,
    },
    PlanCompleted {
        plan_id: String,
        file: String,
        phase: String,
        actions_applied: usize,
    },
    PlanSkipped {
        plan_id: String,
        file: String,
        phase: String,
        skip_reason: String,
    },
    PlanFailed {
        plan_id: String,
        file: String,
        phase: String,
        /// Single error message. Detailed error chains and subprocess
        /// output are intentionally NOT forwarded over SSE — they need a
        /// separate disclosure review before exposing subprocess output to web
        /// clients.
        error: String,
    },
}

/// Application state shared across all handlers.
#[non_exhaustive]
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn StorageTrait>,
    pub sse_tx: broadcast::Sender<SseEvent>,
    pub templates: Arc<tera::Tera>,
    auth_token: Option<String>,
    pub sse_client_count: Arc<AtomicU32>,
    pub plugin_info: Arc<Vec<crate::api::plugins::PluginInfoResponse>>,
    pub data_dir: Option<std::path::PathBuf>,
    pub process_runner: Option<Arc<dyn ProcessRunLauncher>>,
}

impl AppState {
    /// Create a new `AppState`.
    ///
    /// The caller must supply a `broadcast::Sender<SseEvent>`. Use
    /// [`AppState::new_with_default_sse`] for tests or other callers that
    /// do not need to share the sender with another component.
    pub fn new(
        store: Arc<dyn StorageTrait>,
        sse_tx: broadcast::Sender<SseEvent>,
        templates: tera::Tera,
        auth_token: Option<String>,
        data_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            store,
            sse_tx,
            templates: Arc::new(templates),
            auth_token,
            sse_client_count: Arc::new(AtomicU32::new(0)),
            plugin_info: Arc::new(Vec::new()),
            data_dir,
            process_runner: None,
        }
    }

    /// Create a new `AppState` with an internally-allocated SSE broadcast
    /// channel of the default capacity. Convenience constructor for tests
    /// and callers that do not need to share the sender.
    #[must_use]
    pub fn new_with_default_sse(
        store: Arc<dyn StorageTrait>,
        templates: tera::Tera,
        auth_token: Option<String>,
        data_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let (sse_tx, _) = broadcast::channel(SSE_CHANNEL_CAPACITY);
        Self::new(store, sse_tx, templates, auth_token, data_dir)
    }

    /// Set the plugin info snapshot (typically populated from kernel registry at startup).
    #[must_use]
    pub fn with_plugin_info(mut self, info: Vec<crate::api::plugins::PluginInfoResponse>) -> Self {
        self.plugin_info = Arc::new(info);
        self
    }

    /// Set the process runner used by `/api/process-runs`.
    #[must_use]
    pub fn with_process_runner(mut self, runner: Arc<dyn ProcessRunLauncher>) -> Self {
        self.process_runner = Some(runner);
        self
    }

    /// Returns true if an auth token is configured.
    #[must_use]
    pub fn has_auth(&self) -> bool {
        self.auth_token.is_some()
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

/// Create an `AppState` with an in-memory store for testing.
#[cfg(test)]
pub(crate) fn make_test_state(auth_token: Option<String>) -> AppState {
    use voom_domain::test_support::InMemoryStore;
    let store = Arc::new(InMemoryStore::new());
    let templates = tera::Tera::default();
    AppState::new_with_default_sse(store, templates, auth_token, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(auth_token: Option<String>) -> AppState {
        make_test_state(auth_token)
    }

    #[test]
    fn test_new_creates_state_with_broadcast_channel() {
        let state = make_state(None);
        // sse_tx should be usable: subscribing should succeed
        let _rx = state.sse_tx.subscribe();
        assert!(!state.has_auth());
    }

    #[test]
    fn test_new_with_auth_token() {
        let state = make_state(Some("my-secret".into()));
        assert!(state.has_auth());
    }

    #[test]
    fn test_sse_client_count_starts_at_zero() {
        let state = make_state(None);
        assert_eq!(
            state
                .sse_client_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn test_is_authorized_no_token_configured_allows_all() {
        let state = make_state(None);
        assert!(state.is_authorized(None));
        assert!(state.is_authorized(Some("Bearer anything")));
        assert!(state.is_authorized(Some("garbage")));
    }

    #[test]
    fn test_is_authorized_with_token_requires_bearer_prefix() {
        let state = make_state(Some("secret123".into()));
        assert!(state.is_authorized(Some("Bearer secret123")));
        assert!(!state.is_authorized(Some("secret123")));
        assert!(!state.is_authorized(Some("bearer secret123")));
        assert!(!state.is_authorized(Some("Token secret123")));
    }

    #[test]
    fn test_is_authorized_with_token_rejects_none() {
        let state = make_state(Some("secret123".into()));
        assert!(!state.is_authorized(None));
    }

    #[test]
    fn test_is_authorized_with_token_rejects_wrong_token() {
        let state = make_state(Some("secret123".into()));
        assert!(!state.is_authorized(Some("Bearer wrong")));
        assert!(!state.is_authorized(Some("Bearer secret1234")));
        assert!(!state.is_authorized(Some("Bearer ")));
    }

    #[test]
    fn test_state_is_clone() {
        let state = make_state(Some("tok".into()));
        let cloned = state.clone();
        assert!(cloned.has_auth());
        // Cloned state shares the same Arc references
        assert!(Arc::ptr_eq(&state.templates, &cloned.templates));
        assert!(Arc::ptr_eq(
            &state.sse_client_count,
            &cloned.sse_client_count
        ));
    }

    #[test]
    fn test_sse_broadcast_delivers_events() {
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
    fn test_sse_event_serialization_tagged() {
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
    fn test_sse_event_plan_executing_serialization() {
        let event = SseEvent::PlanExecuting {
            plan_id: "p-1".into(),
            file: "movie.mkv".into(),
            phase: "transcode".into(),
            action_count: 3,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "PlanExecuting");
        assert_eq!(json["data"]["plan_id"], "p-1");
        assert_eq!(json["data"]["file"], "movie.mkv");
        assert_eq!(json["data"]["phase"], "transcode");
        assert_eq!(json["data"]["action_count"], 3);
    }

    #[test]
    fn test_sse_event_plan_completed_serialization() {
        let event = SseEvent::PlanCompleted {
            plan_id: "p-2".into(),
            file: "movie.mkv".into(),
            phase: "remux".into(),
            actions_applied: 5,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "PlanCompleted");
        assert_eq!(json["data"]["plan_id"], "p-2");
        assert_eq!(json["data"]["file"], "movie.mkv");
        assert_eq!(json["data"]["phase"], "remux");
        assert_eq!(json["data"]["actions_applied"], 5);
    }

    #[test]
    fn test_sse_event_plan_skipped_serialization() {
        let event = SseEvent::PlanSkipped {
            plan_id: "p-3".into(),
            file: "movie.mkv".into(),
            phase: "transcode".into(),
            skip_reason: "no matching tracks".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "PlanSkipped");
        assert_eq!(json["data"]["plan_id"], "p-3");
        assert_eq!(json["data"]["file"], "movie.mkv");
        assert_eq!(json["data"]["phase"], "transcode");
        assert_eq!(json["data"]["skip_reason"], "no matching tracks");
    }

    #[test]
    fn test_sse_event_plan_failed_serialization() {
        let event = SseEvent::PlanFailed {
            plan_id: "p-4".into(),
            file: "movie.mkv".into(),
            phase: "transcode".into(),
            error: "ffmpeg returned non-zero".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "PlanFailed");
        assert_eq!(json["data"]["plan_id"], "p-4");
        assert_eq!(json["data"]["file"], "movie.mkv");
        assert_eq!(json["data"]["phase"], "transcode");
        assert_eq!(json["data"]["error"], "ffmpeg returned non-zero");
        // Confirm no leak-prone fields appear in the serialized envelope.
        assert!(json["data"].get("error_chain").is_none());
        assert!(json["data"].get("execution_detail").is_none());
    }

    #[test]
    fn test_sse_event_job_completed_serialization() {
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
