use serde::{Deserialize, Serialize};

/// Describes what a plugin can do. The kernel uses these for capability-based routing.
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
        operations: Vec<String>,
        formats: Vec<String>,
    },
    Store {
        backend: String,
    },
    DetectTools,
    ManageJobs,
    ServeHttp,
    Orchestrate,
    Backup,
    EnrichMetadata {
        source: String,
    },
    Transcribe,
    Synthesize,
}

impl Capability {
    /// Returns the capability kind as a string for matching.
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
            Capability::Orchestrate => "orchestrate",
            Capability::Backup => "backup",
            Capability::EnrichMetadata { .. } => "enrich_metadata",
            Capability::Transcribe => "transcribe",
            Capability::Synthesize => "synthesize",
        }
    }

    /// Check if this capability can handle the given operation.
    pub fn supports_operation(&self, operation: &str) -> bool {
        match self {
            Capability::Execute { operations, .. } => {
                operations.is_empty() || operations.iter().any(|o| o == operation)
            }
            _ => false,
        }
    }

    /// Check if this capability can handle the given format.
    pub fn supports_format(&self, format: &str) -> bool {
        match self {
            Capability::Introspect { formats } | Capability::Execute { formats, .. } => {
                formats.is_empty() || formats.iter().any(|f| f == format)
            }
            Capability::Discover { schemes } => {
                schemes.is_empty() || schemes.iter().any(|s| s == format)
            }
            _ => true,
        }
    }
}
