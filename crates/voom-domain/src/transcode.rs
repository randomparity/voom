use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted outcome from a VMAF-guided transcode run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscodeOutcome {
    pub id: Uuid,
    pub file_id: String,
    pub target_vmaf: Option<u32>,
    pub achieved_vmaf: Option<f32>,
    pub crf_used: Option<u32>,
    pub bitrate_used: Option<String>,
    pub iterations: u32,
    pub sample_strategy: SampleStrategy,
    pub fallback_used: bool,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SampleStrategy {
    Full,
    Uniform { count: u32, duration: String },
    Scenes { count: u32, duration: String },
}
