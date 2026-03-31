use serde::{Deserialize, Serialize};

use crate::plan::OperationType;

/// Describes what a plugin can do. The kernel uses these for capability-based routing.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Capability {
    Discover {
        schemes: Vec<String>,
    },
    Introspect {
        formats: Vec<String>,
    },
    Evaluate,
    Execute {
        operations: Vec<OperationType>,
        formats: Vec<String>,
    },
    Store {
        backend: String,
    },
    DetectTools,
    ManageJobs,
    ServeHttp,
    Plan,
    Backup,
    EnrichMetadata {
        source: String,
    },
    Transcribe,
    Synthesize,
    GenerateSubtitle,
    HealthCheck,
}

impl Capability {
    /// Returns the capability kind as a string for matching.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Capability::Discover { .. } => "discover",
            Capability::Introspect { .. } => "introspect",
            Capability::Evaluate => "evaluate",
            Capability::Execute { .. } => "execute",
            Capability::Store { .. } => "store",
            Capability::DetectTools => "detect_tools",
            Capability::ManageJobs => "manage_jobs",
            Capability::ServeHttp => "serve_http",
            Capability::Plan => "plan",
            Capability::Backup => "backup",
            Capability::EnrichMetadata { .. } => "enrich_metadata",
            Capability::Transcribe => "transcribe",
            Capability::Synthesize => "synthesize",
            Capability::GenerateSubtitle => "generate_subtitle",
            Capability::HealthCheck => "health_check",
        }
    }
}
