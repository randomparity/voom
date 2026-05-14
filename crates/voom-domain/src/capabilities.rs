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
    EvaluatePolicy,
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
    OrchestratePhases,
    Backup,
    EnrichMetadata {
        source: String,
    },
    Transcribe,
    Synthesize,
    GenerateSubtitle,
    HealthCheck,
    Verify {
        modes: Vec<crate::verification::VerificationMode>,
    },
}

impl Capability {
    /// Returns the capability kind as a string for matching.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Capability::Discover { .. } => "discover",
            Capability::Introspect { .. } => "introspect",
            Capability::EvaluatePolicy => "evaluate_policy",
            Capability::Execute { .. } => "execute",
            Capability::Store { .. } => "store",
            Capability::DetectTools => "detect_tools",
            Capability::ManageJobs => "manage_jobs",
            Capability::ServeHttp => "serve_http",
            Capability::OrchestratePhases => "orchestrate_phases",
            Capability::Backup => "backup",
            Capability::EnrichMetadata { .. } => "enrich_metadata",
            Capability::Transcribe => "transcribe",
            Capability::Synthesize => "synthesize",
            Capability::GenerateSubtitle => "generate_subtitle",
            Capability::HealthCheck => "health_check",
            Capability::Verify { .. } => "verify",
        }
    }

    #[must_use]
    pub fn resolution(&self) -> crate::capability_resolution::CapabilityResolution {
        use crate::capability_resolution::CapabilityResolution as R;
        match self {
            Capability::Discover { .. } => R::Sharded,
            Capability::Introspect { .. } => R::Competing,
            Capability::EvaluatePolicy => R::Exclusive,
            Capability::Execute { .. } => R::Competing,
            Capability::Store { .. } => R::Exclusive,
            Capability::DetectTools => R::Competing,
            Capability::ManageJobs => R::Exclusive,
            Capability::ServeHttp => R::Exclusive,
            Capability::OrchestratePhases => R::Exclusive,
            Capability::Backup => R::Competing,
            Capability::EnrichMetadata { .. } => R::Sharded,
            Capability::Transcribe => R::Competing,
            Capability::Synthesize => R::Competing,
            Capability::GenerateSubtitle => R::Competing,
            Capability::HealthCheck => R::Competing,
            Capability::Verify { .. } => R::Competing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_capability_kind() {
        use crate::verification::VerificationMode;
        let cap = Capability::Verify {
            modes: vec![VerificationMode::Quick, VerificationMode::Thorough],
        };
        assert_eq!(cap.kind(), "verify");
    }

    #[test]
    fn discover_resolution_is_sharded() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::Discover { schemes: vec!["file".into()] };
        assert_eq!(cap.resolution(), CapabilityResolution::Sharded);
    }

    #[test]
    fn evaluate_resolution_is_exclusive() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::EvaluatePolicy;
        assert_eq!(cap.resolution(), CapabilityResolution::Exclusive);
    }

    #[test]
    fn orchestrate_phases_resolution_is_exclusive() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::OrchestratePhases;
        assert_eq!(cap.resolution(), CapabilityResolution::Exclusive);
    }

    #[test]
    fn introspect_resolution_is_competing() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::Introspect { formats: vec!["mkv".into()] };
        assert_eq!(cap.resolution(), CapabilityResolution::Competing);
    }

    #[test]
    fn store_resolution_is_exclusive() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::Store { backend: "sqlite".into() };
        assert_eq!(cap.resolution(), CapabilityResolution::Exclusive);
    }
}
