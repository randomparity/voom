use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::media::MediaFile;
use crate::plan::Plan;

/// All event types that flow through the event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    FileIntrospected(FileIntrospectedEvent),
    MetadataEnriched(MetadataEnrichedEvent),
    PolicyEvaluate(PolicyEvaluateEvent),
    PlanCreated(PlanCreatedEvent),
    PlanExecuting(PlanExecutingEvent),
    PlanCompleted(PlanCompletedEvent),
    PlanFailed(PlanFailedEvent),
    JobStarted(JobStartedEvent),
    JobProgress(JobProgressEvent),
    JobCompleted(JobCompletedEvent),
    ToolDetected(ToolDetectedEvent),
}

impl Event {
    /// Returns the event type string used for subscription matching.
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
        }
    }
}

/// Result returned by a plugin after processing an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventResult {
    pub plugin_name: String,
    pub produced_events: Vec<Event>,
    pub data: Option<serde_json::Value>,
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
    pub path: PathBuf,
    pub phase_name: String,
    pub actions_applied: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFailedEvent {
    pub path: PathBuf,
    pub phase_name: String,
    pub error: String,
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
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobCompletedEvent {
    pub job_id: String,
    pub success: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetectedEvent {
    pub tool_name: String,
    pub version: String,
    pub path: PathBuf,
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
}
