//! Request/response messages for kernel-routed plugin calls.
//!
//! `Call` is distinct from `Event`: point-to-point (single handler, not
//! broadcast), not subject to bus claim/cascade semantics, not written to
//! `event_log`, and its handler returns exactly one `CallResponse`.
//!
//! Some `Call` variants are **streaming** — their payload carries an
//! `mpsc::Sender` the handler writes items to during execution. The Call still
//! returns a single `CallResponse` (typically a summary) on completion.

use std::collections::HashMap;

use crate::capability_map::CapabilityMap;
use crate::compiled::CompiledPolicy;
use crate::evaluation::EvaluationOutcome;
use crate::media::MediaFile;
use crate::plan::{PhaseOutput, Plan};

/// A request addressed to exactly one plugin via its capability.
#[non_exhaustive]
#[derive(Debug)]
pub enum Call {
    /// Evaluate a compiled policy against a media file. Unary Call.
    EvaluatePolicy {
        policy: CompiledPolicy,
        file: MediaFile,
        phase: Option<String>,
        phase_outputs: Option<HashMap<String, PhaseOutput>>,
        phase_outcomes: Option<HashMap<String, EvaluationOutcome>>,
        capabilities_override: Option<CapabilityMap>,
    },
    /// Orchestrate phases from pre-evaluated plans. Unary Call.
    Orchestrate {
        plans: Vec<Plan>,
        policy_name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiled::{CompiledConfig, CompiledMetadata, ErrorStrategy};
    use std::path::PathBuf;

    /// Construct a minimal `CompiledPolicy` directly via its public
    /// constructors. We deliberately avoid `voom_dsl::compile_policy` here:
    /// adding `voom-dsl` as a dev-dependency would create two distinct
    /// instances of `voom-domain` in the test binary (voom-dsl re-exports
    /// CompiledPolicy from voom-domain, so types fail to unify).
    fn minimal_policy() -> CompiledPolicy {
        CompiledPolicy::new(
            "demo".into(),
            CompiledMetadata::default(),
            CompiledConfig::new(vec![], vec![], ErrorStrategy::Abort, vec![], false),
            vec![],
            vec![],
            String::new(),
        )
    }

    #[test]
    fn evaluate_policy_call_constructs() {
        let call = Call::EvaluatePolicy {
            policy: minimal_policy(),
            file: MediaFile::new(PathBuf::from("/x.mkv")),
            phase: None,
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let dbg = format!("{call:?}");
        assert!(dbg.contains("EvaluatePolicy"));
    }

    #[test]
    fn orchestrate_call_constructs() {
        let call = Call::Orchestrate {
            plans: vec![],
            policy_name: "demo".into(),
        };
        let dbg = format!("{call:?}");
        assert!(dbg.contains("Orchestrate"));
    }
}
