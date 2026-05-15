//! Wasm-boundary mirror of `Call`. Omits non-serde fields (in-process
//! resources like `mpsc::Sender`, `CancellationToken`, and closure-bearing
//! `ScanOptions` fields) so the typed Call can cross to WASM plugins as
//! MessagePack. `CallResponse` is already serde-clean and crosses the
//! boundary unchanged via `crate::call::CallResponse`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::capability_map::CapabilityMap;
use crate::compiled::CompiledPolicy;
use crate::evaluation::EvaluationOutcome;
use crate::media::MediaFile;
use crate::plan::{PhaseOutput, Plan};
use crate::scan::ScanOptions;
use crate::transition::ScanSessionId;

/// The Wasm-safe portion of a `Call`. Streaming-Call payloads omit `sink`,
/// `root_done`, `cancel`; closure-bearing ScanOptions fields are also dropped
/// (only the data fields cross the boundary).
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WasmCall {
    EvaluatePolicy {
        policy: Box<CompiledPolicy>,
        file: Box<MediaFile>,
        phase: Option<String>,
        phase_outputs: Option<HashMap<String, PhaseOutput>>,
        phase_outcomes: Option<HashMap<String, EvaluationOutcome>>,
        capabilities_override: Option<CapabilityMap>,
    },
    Orchestrate {
        plans: Vec<Plan>,
        policy_name: String,
    },
    ScanLibrary {
        uri: String,
        options: WasmScanOptions,
        scan_session: ScanSessionId,
    },
}

/// The serde-clean subset of `ScanOptions`. Drops closure fields
/// (`fingerprint_lookup`, `session_mutations`, `on_error`, `on_progress`) —
/// those are host-only and not available to WASM plugins in this phase.
/// A follow-up issue (see spec §7 "Explicit WASM limitations") may add
/// host fns to bridge them.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmScanOptions {
    pub root: std::path::PathBuf,
    pub recursive: bool,
    pub hash_files: bool,
    pub workers: usize,
}

impl WasmScanOptions {
    /// Project the Wasm-safe subset out of a full `ScanOptions`.
    #[must_use]
    pub fn from_scan_options(o: &ScanOptions) -> Self {
        Self {
            root: o.root.clone(),
            recursive: o.recursive,
            hash_files: o.hash_files,
            workers: o.workers,
        }
    }
}

impl WasmCall {
    /// Convert a `Call` to its Wasm-safe mirror.
    #[must_use]
    pub fn from_call(call: &crate::call::Call) -> Self {
        use crate::call::Call;
        match call {
            Call::EvaluatePolicy {
                policy,
                file,
                phase,
                phase_outputs,
                phase_outcomes,
                capabilities_override,
            } => WasmCall::EvaluatePolicy {
                policy: policy.clone(),
                file: file.clone(),
                phase: phase.clone(),
                phase_outputs: phase_outputs.clone(),
                phase_outcomes: phase_outcomes.clone(),
                capabilities_override: capabilities_override.clone(),
            },
            Call::Orchestrate { plans, policy_name } => WasmCall::Orchestrate {
                plans: plans.clone(),
                policy_name: policy_name.clone(),
            },
            Call::ScanLibrary {
                uri,
                options,
                scan_session,
                ..
            } => WasmCall::ScanLibrary {
                uri: uri.clone(),
                options: WasmScanOptions::from_scan_options(options),
                scan_session: *scan_session,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::Call;
    use crate::compiled::{CompiledConfig, CompiledMetadata, ErrorStrategy};
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    /// Construct a minimal CompiledPolicy without depending on voom-dsl
    /// (see crates/voom-domain/src/call.rs:119-123 for why).
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
    fn evaluate_policy_roundtrips_via_wasm_mirror() {
        let original = Call::EvaluatePolicy {
            policy: Box::new(minimal_policy()),
            file: Box::new(MediaFile::new(PathBuf::from("/x.mkv"))),
            phase: Some("init".into()),
            phase_outputs: None,
            phase_outcomes: None,
            capabilities_override: None,
        };
        let wasm = WasmCall::from_call(&original);
        let bytes = rmp_serde::to_vec_named(&wasm).expect("serialize");
        let back: WasmCall = rmp_serde::from_slice(&bytes).expect("deserialize");
        match back {
            WasmCall::EvaluatePolicy { phase, .. } => assert_eq!(phase.as_deref(), Some("init")),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn scan_library_drops_non_serde_fields() {
        let (sink, _rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let original = Call::ScanLibrary {
            uri: "file:///tmp".into(),
            options: ScanOptions::new("/tmp"),
            scan_session: ScanSessionId::new(),
            sink,
            root_done: None,
            cancel,
        };
        let wasm = WasmCall::from_call(&original);
        match wasm {
            WasmCall::ScanLibrary {
                uri,
                options,
                scan_session: _,
            } => {
                assert_eq!(uri, "file:///tmp");
                // ScanOptions::new sets hash_files: true (see scan.rs rev-3 default).
                assert!(options.hash_files);
                assert!(options.recursive);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn orchestrate_roundtrips_via_wasm_mirror() {
        let bytes = rmp_serde::to_vec_named(&WasmCall::from_call(&Call::Orchestrate {
            plans: vec![],
            policy_name: "demo".into(),
        }))
        .expect("ser");
        let back: WasmCall = rmp_serde::from_slice(&bytes).expect("de");
        match back {
            WasmCall::Orchestrate { policy_name, .. } => assert_eq!(policy_name, "demo"),
            _ => panic!("wrong variant"),
        }
    }
}
