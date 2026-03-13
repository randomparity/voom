use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use crate::media::MediaFile;
use crate::plan::Plan;

/// All event types that flow through the event bus.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    FileIntrospected(FileIntrospectedEvent),
    /// Emitted by WASM metadata plugins. Consumed by the sqlite-store plugin
    /// to persist enriched metadata as plugin data keyed by file path.
    MetadataEnriched(MetadataEnrichedEvent),
    PolicyEvaluate(PolicyEvaluateEvent),
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
}

impl Event {
    /// Returns the event type string used for subscription matching.
    #[must_use]
    pub fn event_type(&self) -> &str {
        match self {
            Event::FileDiscovered(_) => "file.discovered",
            Event::FileIntrospected(_) => "file.introspected",
            Event::MetadataEnriched(_) => "metadata.enriched",
            Event::PolicyEvaluate(_) => "policy.evaluate",
            Event::PlanCreated(_) => "plan.created",
            Event::PlanExecuting(_) => "plan.executing",
            Event::PlanCompleted(_) => "plan.completed",
            Event::PlanFailed(_) => "plan.failed",
            Event::JobStarted(_) => "job.started",
            Event::JobProgress(_) => "job.progress",
            Event::JobCompleted(_) => "job.completed",
            Event::ToolDetected(_) => "tool.detected",
            Event::PluginError(_) => "plugin.error",
        }
    }
}

/// Result returned by a plugin after processing an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResult {
    pub plugin_name: String,
    pub produced_events: Vec<Event>,
    pub data: Option<serde_json::Value>,
    /// When `true`, the event bus stops dispatching this event to lower-priority
    /// handlers. Produced events from the claiming result still cascade normally.
    #[serde(default)]
    pub claimed: bool,
}

impl EventResult {
    /// Build the standard pair of lifecycle events that executor plugins emit
    /// when a plan execution succeeds: `PlanExecuting` + `PlanCompleted`.
    pub fn plan_succeeded(
        plugin_name: impl Into<String>,
        plan: &crate::plan::Plan,
        actions_applied: usize,
        data: Option<serde_json::Value>,
    ) -> Self {
        let executing = Event::PlanExecuting(PlanExecutingEvent {
            path: plan.file.path.clone(),
            phase_name: plan.phase_name.clone(),
            action_count: plan.actions.len(),
        });
        let completed = Event::PlanCompleted(PlanCompletedEvent {
            plan_id: plan.id,
            path: plan.file.path.clone(),
            phase_name: plan.phase_name.clone(),
            actions_applied,
        });
        Self {
            plugin_name: plugin_name.into(),
            produced_events: vec![executing, completed],
            data,
            claimed: true,
        }
    }

    /// Build the standard pair of lifecycle events that executor plugins emit
    /// when a plan execution fails: `PlanExecuting` + `PlanFailed`.
    pub fn plan_failed(
        plugin_name: impl Into<String>,
        plan: &crate::plan::Plan,
        error: String,
    ) -> Self {
        let name: String = plugin_name.into();
        let executing = Event::PlanExecuting(PlanExecutingEvent {
            path: plan.file.path.clone(),
            phase_name: plan.phase_name.clone(),
            action_count: plan.actions.len(),
        });
        let failed = Event::PlanFailed(PlanFailedEvent {
            plan_id: plan.id,
            path: plan.file.path.clone(),
            phase_name: plan.phase_name.clone(),
            error,
            error_code: None,
            plugin_name: Some(name.clone()),
            error_chain: Vec::new(),
        });
        Self {
            plugin_name: name,
            produced_events: vec![executing, failed],
            data: None,
            claimed: true,
        }
    }
}

// --- Event payload structs ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiscoveredEvent {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileIntrospectedEvent {
    pub file: MediaFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEnrichedEvent {
    pub path: PathBuf,
    pub source: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEvaluateEvent {
    pub path: PathBuf,
    pub policy_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCreatedEvent {
    pub plan: Plan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutingEvent {
    pub path: PathBuf,
    pub phase_name: String,
    pub action_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCompletedEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub actions_applied: usize,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStartedEvent {
    pub job_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProgressEvent {
    pub job_id: String,
    pub progress: f64,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompletedEvent {
    pub job_id: String,
    pub success: bool,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetectedEvent {
    pub tool_name: String,
    pub version: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginErrorEvent {
    pub plugin_name: String,
    pub event_type: String,
    pub error: String,
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
            job_id: "job-1".into(),
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
        let json = r#"{"job_id":"j1","progress":0.5}"#;
        let event: JobProgressEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.job_id, "j1");
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
    fn test_job_completed_missing_optional_fields() {
        let json = r#"{"job_id":"j2","success":true}"#;
        let event: JobCompletedEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.job_id, "j2");
        assert!(event.success);
        assert!(event.message.is_none());
    }
}
