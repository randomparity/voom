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

    /// Does this capability match the given query?
    ///
    /// rev-3 invariant: the query's resolution class (Exclusive / Sharded /
    /// Competing) must match the capability's `resolution()`. A capability
    /// with a matching `kind()` but the wrong resolution class does NOT
    /// match. This keeps `dispatch_to_capability` from routing to plugins
    /// under the wrong uniqueness/ordering contract.
    #[must_use]
    pub fn matches_query(&self, query: &CapabilityQuery) -> bool {
        use crate::capability_resolution::CapabilityResolution as R;
        match query {
            CapabilityQuery::Exclusive { kind } => {
                self.kind() == kind && self.resolution() == R::Exclusive
            }
            CapabilityQuery::Competing { kind } => {
                self.kind() == kind && self.resolution() == R::Competing
            }
            CapabilityQuery::Sharded { kind, key } => {
                if self.kind() != kind || self.resolution() != R::Sharded {
                    return false;
                }
                match self {
                    Capability::Discover { schemes } => schemes.iter().any(|s| s == key),
                    Capability::EnrichMetadata { source } => source == key,
                    _ => false,
                }
            }
        }
    }
}

/// Caller-side query identifying which capability claim to dispatch to.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum CapabilityQuery {
    /// Match an Exclusive capability by kind.
    Exclusive { kind: String },
    /// Match a Sharded capability by kind and key.
    Sharded { kind: String, key: String },
    /// Match a Competing capability by kind.
    Competing { kind: String },
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
        let cap = Capability::Discover {
            schemes: vec!["file".into()],
        };
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
        let cap = Capability::Introspect {
            formats: vec!["mkv".into()],
        };
        assert_eq!(cap.resolution(), CapabilityResolution::Competing);
    }

    #[test]
    fn store_resolution_is_exclusive() {
        use crate::capability_resolution::CapabilityResolution;
        let cap = Capability::Store {
            backend: "sqlite".into(),
        };
        assert_eq!(cap.resolution(), CapabilityResolution::Exclusive);
    }

    #[test]
    fn capability_query_matches_discover_by_scheme() {
        let plugin_cap = Capability::Discover {
            schemes: vec!["file".into(), "smb".into()],
        };
        assert!(plugin_cap.matches_query(&CapabilityQuery::Sharded {
            kind: "discover".into(),
            key: "file".into(),
        }));
        assert!(plugin_cap.matches_query(&CapabilityQuery::Sharded {
            kind: "discover".into(),
            key: "smb".into(),
        }));
        assert!(!plugin_cap.matches_query(&CapabilityQuery::Sharded {
            kind: "discover".into(),
            key: "s3".into(),
        }));
    }

    #[test]
    fn capability_query_matches_exclusive() {
        assert!(
            Capability::EvaluatePolicy.matches_query(&CapabilityQuery::Exclusive {
                kind: "evaluate_policy".into(),
            })
        );
        assert!(
            !Capability::EvaluatePolicy.matches_query(&CapabilityQuery::Exclusive {
                kind: "orchestrate_phases".into(),
            })
        );
    }

    // rev-3: matches_query must enforce resolution class, not just kind.

    #[test]
    fn exclusive_query_does_not_match_competing_capability() {
        let cap = Capability::Execute {
            operations: vec![],
            formats: vec!["mkv".into()],
        };
        assert!(!cap.matches_query(&CapabilityQuery::Exclusive {
            kind: "execute".into()
        }));
    }

    #[test]
    fn competing_query_does_not_match_exclusive_capability() {
        let cap = Capability::EvaluatePolicy;
        assert!(!cap.matches_query(&CapabilityQuery::Competing {
            kind: "evaluate_policy".into()
        }));
    }

    #[test]
    fn sharded_query_does_not_match_exclusive_capability() {
        let cap = Capability::EvaluatePolicy;
        assert!(!cap.matches_query(&CapabilityQuery::Sharded {
            kind: "evaluate_policy".into(),
            key: "x".into(),
        }));
    }

    #[test]
    fn exclusive_query_does_not_match_sharded_capability() {
        let cap = Capability::Discover {
            schemes: vec!["file".into()],
        };
        assert!(!cap.matches_query(&CapabilityQuery::Exclusive {
            kind: "discover".into()
        }));
    }

    #[test]
    fn correct_resolution_query_matches() {
        let cap = Capability::Execute {
            operations: vec![],
            formats: vec!["mkv".into()],
        };
        assert!(
            cap.matches_query(&CapabilityQuery::Competing {
                kind: "execute".into()
            }),
            "a Competing query against a Competing capability with the same kind must match"
        );
    }
}
