use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::bad_file::BadFileSource;
use crate::media::MediaFile;
use crate::plan::{ActionResult, Plan};

/// All event types that flow through the event bus.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    FileIntrospected(FileIntrospectedEvent),
    FileIntrospectionFailed(FileIntrospectionFailedEvent),
    /// Emitted by WASM metadata plugins. Consumed by the sqlite-store plugin
    /// to persist enriched metadata as plugin data keyed by file path.
    MetadataEnriched(MetadataEnrichedEvent),
    PlanCreated(PlanCreatedEvent),
    PlanExecuting(PlanExecutingEvent),
    PlanCompleted(PlanCompletedEvent),
    PlanFailed(PlanFailedEvent),
    JobStarted(JobStartedEvent),
    JobProgress(JobProgressEvent),
    JobCompleted(JobCompletedEvent),
    /// Emitted by the tool-detector plugin. Consumed by the sqlite-store plugin
    /// to persist tool info, exposed via the web server's GET /api/tools endpoint.
    ToolDetected(ToolDetectedEvent),
    PluginError(PluginErrorEvent),
    HealthStatus(HealthStatusEvent),
}

impl Event {
    // ── Event type constants ────────────────────────────────────
    // Use these instead of string literals in Plugin::handles() implementations
    // to get compile-time typo protection.
    pub const FILE_DISCOVERED: &str = "file.discovered";
    pub const FILE_INTROSPECTED: &str = "file.introspected";
    pub const FILE_INTROSPECTION_FAILED: &str = "file.introspection_failed";
    pub const METADATA_ENRICHED: &str = "metadata.enriched";
    pub const PLAN_CREATED: &str = "plan.created";
    pub const PLAN_EXECUTING: &str = "plan.executing";
    pub const PLAN_COMPLETED: &str = "plan.completed";
    pub const PLAN_FAILED: &str = "plan.failed";
    pub const JOB_STARTED: &str = "job.started";
    pub const JOB_PROGRESS: &str = "job.progress";
    pub const JOB_COMPLETED: &str = "job.completed";
    pub const TOOL_DETECTED: &str = "tool.detected";
    pub const PLUGIN_ERROR: &str = "plugin.error";
    pub const HEALTH_STATUS: &str = "health.status";

    /// Returns the event type string used for subscription matching.
    #[must_use]
    pub fn event_type(&self) -> &str {
        match self {
            Event::FileDiscovered(_) => Self::FILE_DISCOVERED,
            Event::FileIntrospected(_) => Self::FILE_INTROSPECTED,
            Event::FileIntrospectionFailed(_) => Self::FILE_INTROSPECTION_FAILED,
            Event::MetadataEnriched(_) => Self::METADATA_ENRICHED,
            Event::PlanCreated(_) => Self::PLAN_CREATED,
            Event::PlanExecuting(_) => Self::PLAN_EXECUTING,
            Event::PlanCompleted(_) => Self::PLAN_COMPLETED,
            Event::PlanFailed(_) => Self::PLAN_FAILED,
            Event::JobStarted(_) => Self::JOB_STARTED,
            Event::JobProgress(_) => Self::JOB_PROGRESS,
            Event::JobCompleted(_) => Self::JOB_COMPLETED,
            Event::ToolDetected(_) => Self::TOOL_DETECTED,
            Event::PluginError(_) => Self::PLUGIN_ERROR,
            Event::HealthStatus(_) => Self::HEALTH_STATUS,
        }
    }
}

/// Result returned by a plugin after processing an event.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResult {
    pub plugin_name: String,
    pub produced_events: Vec<Event>,
    pub data: Option<serde_json::Value>,
    /// When `true`, the event bus stops dispatching this event to lower-priority
    /// handlers. Produced events from the claiming result still cascade normally.
    #[serde(default)]
    pub claimed: bool,
    /// Set when an executor claims a plan but fails to execute it.
    /// Allows callers to distinguish claimed+succeeded from claimed+failed
    /// without parsing the `data` JSON.
    #[serde(default)]
    pub execution_error: Option<String>,
}

impl EventResult {
    #[must_use]
    pub fn new(plugin_name: impl Into<String>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data: None,
            claimed: false,
            execution_error: None,
        }
    }
}

impl EventResult {
    /// Build a result for executor plugins when a plan execution succeeds.
    ///
    /// Lifecycle events (`PlanExecuting`, `PlanCompleted`) are dispatched by the
    /// orchestrator in `process.rs`, not produced by executors, to avoid
    /// duplicate dispatches.
    pub fn plan_succeeded(plugin_name: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data,
            claimed: true,
            execution_error: None,
        }
    }

    /// Build a result for executor plugins when a plan execution fails.
    ///
    /// Lifecycle events (`PlanExecuting`, `PlanFailed`) are dispatched by the
    /// orchestrator in `process.rs`, not produced by executors, to avoid
    /// duplicate dispatches.
    pub fn plan_failed(plugin_name: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![],
            data: None,
            claimed: true,
            execution_error: Some(error.into()),
        }
    }

    /// Wrap the outcome of an executor's plan execution into an `EventResult`.
    ///
    /// On success the result carries the action results as JSON data and is
    /// marked as claimed.  On failure a failed result is returned (callers
    /// should log the error if needed).
    #[must_use]
    pub fn from_plan_execution(
        plugin_name: &str,
        outcome: crate::errors::Result<Vec<ActionResult>>,
    ) -> Self {
        match outcome {
            Ok(results) => {
                let actions_applied = results.iter().filter(|r| r.success).count();
                Self::plan_succeeded(
                    plugin_name,
                    Some(serde_json::json!({
                        "actions_applied": actions_applied,
                        "results": serde_json::to_value(&results).unwrap_or_default(),
                    })),
                )
            }
            Err(e) => Self::plan_failed(plugin_name, e.to_string()),
        }
    }
}

// --- Event payload structs ---

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiscoveredEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
}

impl FileDiscoveredEvent {
    #[must_use]
    pub fn new(path: PathBuf, size: u64, content_hash: String) -> Self {
        Self {
            path,
            size,
            content_hash,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectedEvent {
    pub file: MediaFile,
}

impl FileIntrospectedEvent {
    #[must_use]
    pub fn new(file: MediaFile) -> Self {
        Self { file }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectionFailedEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: Option<String>,
    pub error: String,
    pub error_source: BadFileSource,
}

impl FileIntrospectionFailedEvent {
    #[must_use]
    pub fn new(
        path: PathBuf,
        size: u64,
        content_hash: Option<String>,
        error: String,
        error_source: BadFileSource,
    ) -> Self {
        Self {
            path,
            size,
            content_hash,
            error,
            error_source,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEnrichedEvent {
    pub path: PathBuf,
    pub source: String,
    pub metadata: serde_json::Value,
}

impl MetadataEnrichedEvent {
    #[must_use]
    pub fn new(path: PathBuf, source: String, metadata: serde_json::Value) -> Self {
        Self {
            path,
            source,
            metadata,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCreatedEvent {
    pub plan: Plan,
}

impl PlanCreatedEvent {
    #[must_use]
    pub fn new(plan: Plan) -> Self {
        Self { plan }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutingEvent {
    pub path: PathBuf,
    pub phase_name: String,
    pub action_count: usize,
}

impl PlanExecutingEvent {
    #[must_use]
    pub fn new(path: PathBuf, phase_name: impl Into<String>, action_count: usize) -> Self {
        Self {
            path,
            phase_name: phase_name.into(),
            action_count,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCompletedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub actions_applied: usize,
}

impl PlanCompletedEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        actions_applied: usize,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            actions_applied,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFailedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub error: String,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub plugin_name: Option<String>,
    /// Chain of causal errors from source to root cause.
    /// Populated when structured error information is available.
    #[serde(default)]
    pub error_chain: Vec<String>,
}

impl PlanFailedEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            error: error.into(),
            error_code: None,
            plugin_name: None,
            error_chain: Vec::new(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStartedEvent {
    pub job_id: Uuid,
    pub description: String,
}

impl JobStartedEvent {
    #[must_use]
    pub fn new(job_id: Uuid, description: impl Into<String>) -> Self {
        Self {
            job_id,
            description: description.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgressEvent {
    pub job_id: Uuid,
    pub progress: f64,
    #[serde(default)]
    pub message: Option<String>,
}

impl JobProgressEvent {
    #[must_use]
    pub fn new(job_id: Uuid, progress: f64) -> Self {
        Self {
            job_id,
            progress,
            message: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompletedEvent {
    pub job_id: Uuid,
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
}

impl JobCompletedEvent {
    #[must_use]
    pub fn new(job_id: Uuid, success: bool) -> Self {
        Self {
            job_id,
            success,
            message: None,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetectedEvent {
    pub tool_name: String,
    pub version: String,
    pub path: PathBuf,
}

impl ToolDetectedEvent {
    #[must_use]
    pub fn new(tool_name: impl Into<String>, version: impl Into<String>, path: PathBuf) -> Self {
        Self {
            tool_name: tool_name.into(),
            version: version.into(),
            path,
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginErrorEvent {
    pub plugin_name: String,
    pub event_type: String,
    pub error: String,
}

impl PluginErrorEvent {
    #[must_use]
    pub fn new(
        plugin_name: impl Into<String>,
        event_type: impl Into<String>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            plugin_name: plugin_name.into(),
            event_type: event_type.into(),
            error: error.into(),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatusEvent {
    pub check_name: String,
    pub passed: bool,
    pub details: Option<String>,
}

impl HealthStatusEvent {
    #[must_use]
    pub fn new(check_name: impl Into<String>, passed: bool, details: Option<String>) -> Self {
        Self {
            check_name: check_name.into(),
            passed,
            details,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_strings() {
        let event = Event::FileDiscovered(FileDiscoveredEvent {
            path: PathBuf::from("/test.mkv"),
            size: 1024,
            content_hash: "abc".into(),
        });
        assert_eq!(event.event_type(), "file.discovered");

        let event = Event::PlanExecuting(PlanExecutingEvent {
            path: PathBuf::from("/test.mkv"),
            phase_name: "normalize".into(),
            action_count: 3,
        });
        assert_eq!(event.event_type(), "plan.executing");
    }

    #[test]
    fn test_event_serde_roundtrip() {
        let event = Event::ToolDetected(ToolDetectedEvent {
            tool_name: "ffprobe".into(),
            version: "6.1".into(),
            path: PathBuf::from("/usr/bin/ffprobe"),
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "tool.detected");
    }

    #[test]
    fn test_event_msgpack_roundtrip() {
        let event = Event::JobProgress(JobProgressEvent {
            job_id: Uuid::new_v4(),
            progress: 0.75,
            message: Some("Processing...".into()),
        });
        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "job.progress");
    }

    #[test]
    fn test_job_progress_missing_optional_fields() {
        // Simulate deserializing from a payload that omits the optional `message` field.
        let id = Uuid::new_v4();
        let json = format!(r#"{{"job_id":"{id}","progress":0.5}}"#);
        let event: JobProgressEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.job_id, id);
        assert!(event.message.is_none());
    }

    #[test]
    fn test_plugin_error_event_serde_roundtrip() {
        let event = Event::PluginError(PluginErrorEvent {
            plugin_name: "bad-plugin".into(),
            event_type: "file.discovered".into(),
            error: "something went wrong".into(),
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "plugin.error");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "plugin.error");
    }

    #[test]
    fn test_plan_failed_missing_optional_fields() {
        let json = r#"{"plan_id":"00000000-0000-0000-0000-000000000000","path":"/test.mkv","phase_name":"normalize","error":"failed"}"#;
        let event: PlanFailedEvent = serde_json::from_str(json).unwrap();
        assert!(event.error_code.is_none());
        assert!(event.plugin_name.is_none());
    }

    #[test]
    fn test_file_introspection_failed_event_type() {
        let event = Event::FileIntrospectionFailed(FileIntrospectionFailedEvent {
            path: PathBuf::from("/test/bad.mkv"),
            size: 1024,
            content_hash: Some("abc".into()),
            error: "ffprobe failed".into(),
            error_source: BadFileSource::Introspection,
        });
        assert_eq!(event.event_type(), "file.introspection_failed");
    }

    #[test]
    fn test_file_introspection_failed_serde_roundtrip() {
        let event = Event::FileIntrospectionFailed(FileIntrospectionFailedEvent {
            path: PathBuf::from("/test/bad.mkv"),
            size: 2048,
            content_hash: None,
            error: "corrupt header".into(),
            error_source: BadFileSource::Parse,
        });
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "file.introspection_failed");

        let bytes = rmp_serde::to_vec(&event).unwrap();
        let deserialized: Event = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.event_type(), "file.introspection_failed");
    }

    #[test]
    fn test_job_completed_missing_optional_fields() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"job_id":"{id}","success":true}}"#);
        let event: JobCompletedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event.job_id, id);
        assert!(event.success);
        assert!(event.message.is_none());
    }
}
