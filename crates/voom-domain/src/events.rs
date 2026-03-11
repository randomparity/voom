use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// All event types that flow through the event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    FileDiscovered(FileDiscoveredEvent),
    FileIntrospected(FileIntrospectedEvent),
    MetadataEnriched(MetadataEnrichedEvent),
    PlanCreated(PlanCreatedEvent),
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
            Event::PlanCreated(_) => "plan.created",
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
    pub path: PathBuf,
    pub container: String,
    pub track_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEnrichedEvent {
    pub path: PathBuf,
    pub source: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCreatedEvent {
    pub policy_name: String,
    pub phase_name: String,
    pub path: PathBuf,
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
