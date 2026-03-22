use serde::{Deserialize, Serialize};

/// Returns true if the list is empty (wildcard) or contains the given value.
fn list_contains(list: &[String], value: &str) -> bool {
    list.is_empty() || list.iter().any(|item| item == value)
}

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
            Capability::Orchestrate => "orchestrate",
            Capability::Backup => "backup",
            Capability::EnrichMetadata { .. } => "enrich_metadata",
            Capability::Transcribe => "transcribe",
            Capability::Synthesize => "synthesize",
        }
    }

    /// Check if this capability can handle the given operation.
    ///
    /// An empty `operations` `Vec` in the [`Capability::Execute`] variant acts
    /// as a wildcard and matches all operations.
    #[must_use]
    pub fn supports_operation(&self, operation: &str) -> bool {
        match self {
            Capability::Execute { operations, .. } => list_contains(operations, operation),
            _ => false,
        }
    }

    /// Check if this capability can handle the given format.
    ///
    /// An empty `formats` (or `schemes`) `Vec` in the capability variant acts
    /// as a wildcard and matches all formats.
    #[must_use]
    pub fn supports_format(&self, format: &str) -> bool {
        match self {
            Capability::Introspect { formats } | Capability::Execute { formats, .. } => {
                list_contains(formats, format)
            }
            Capability::Discover { schemes } => list_contains(schemes, format),
            // Capabilities without a format concept don't match any format.
            _ => false,
        }
    }
}
