//! Helper for the CLI's two discovery-stage dispatchers (scan/pipeline.rs
//! and process/pipeline_streaming.rs). Validates the `CallResponse` variant
//! returned by `Kernel::dispatch_to_capability`, folds any `summary.errors`
//! the plugin reported into the shared discovery-error counter, and turns a
//! wrong response variant into a hard error.
//!
//! Native discovery currently always returns `errors: vec![]` because all
//! errors flow through the in-process `options.on_error` callback. But the
//! `Call::ScanLibrary` contract allows non-native (e.g. WASM) discovery
//! plugins that report errors only via `summary.errors` — those would
//! otherwise be silently discarded by the CLI.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use voom_domain::call::CallResponse;

/// Handle the result of `kernel.dispatch_to_capability(query, Call::ScanLibrary { ... })`
/// for one root.
///
/// - `Ok(CallResponse::ScanLibrary(summary))`: increments `discovery_errors`
///   by `summary.errors.len()` and returns `Ok(())`.
/// - `Ok(other_variant)`: returns `Err` — the kernel returned a response that
///   doesn't match the request. The caller fails closed.
/// - `Err(e)`: wraps with `"filesystem scan failed for {path}"` context and
///   returns `Err`.
// Tasks 5 and 6 (PR #420 follow-up) wire this helper into scan/pipeline.rs
// and process/pipeline_streaming.rs respectively; until then it is exercised
// only by the unit tests in this module.
#[allow(dead_code)]
pub(crate) fn handle_dispatch_result(
    result: std::result::Result<CallResponse, voom_domain::errors::VoomError>,
    path: &Path,
    discovery_errors: &Arc<AtomicU64>,
) -> Result<()> {
    match result {
        Ok(CallResponse::ScanLibrary(summary)) => {
            let added = u64::try_from(summary.errors.len()).unwrap_or(u64::MAX);
            if added > 0 {
                discovery_errors.fetch_add(added, Ordering::Relaxed);
                tracing::warn!(
                    root = %path.display(),
                    error_count = added,
                    "discovery dispatch reported {added} errors via ScanSummary"
                );
            }
            Ok(())
        }
        Ok(other) => Err(anyhow!(
            "discovery dispatch returned wrong CallResponse variant for {}: {other:?}",
            path.display()
        )),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("filesystem scan failed for {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use voom_domain::call::{CallResponse, DiscoveryError, ScanSummary};

    #[test]
    fn handle_ok_with_empty_errors_does_not_increment_counter() {
        let counter = Arc::new(AtomicU64::new(0));
        let response = Ok(CallResponse::ScanLibrary(ScanSummary::new(7, vec![], 1)));
        let res = handle_dispatch_result(response, &PathBuf::from("/some/root"), &counter);
        assert!(res.is_ok());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn handle_ok_with_summary_errors_folds_into_counter() {
        let counter = Arc::new(AtomicU64::new(0));
        let errs = vec![
            DiscoveryError::new("a".into(), "boom".into()),
            DiscoveryError::new("b".into(), "kaboom".into()),
            DiscoveryError::new("c".into(), "splat".into()),
        ];
        let response = Ok(CallResponse::ScanLibrary(ScanSummary::new(0, errs, 1)));
        let res = handle_dispatch_result(response, &PathBuf::from("/some/root"), &counter);
        assert!(res.is_ok(), "summary errors are recorded, not propagated");
        assert_eq!(
            counter.load(Ordering::Relaxed),
            3,
            "counter must be incremented by summary.errors.len()"
        );
    }

    #[test]
    fn handle_wrong_callresponse_variant_is_hard_error() {
        let counter = Arc::new(AtomicU64::new(0));
        // Orchestrate variant is wrong for a ScanLibrary dispatch.
        let response = Ok(CallResponse::Orchestrate(
            voom_domain::orchestration::OrchestrationResult::new(vec![], vec![], false),
        ));
        let res = handle_dispatch_result(response, &PathBuf::from("/some/root"), &counter);
        let err = res.expect_err("wrong variant must be hard error");
        assert!(
            err.to_string().contains("wrong CallResponse variant"),
            "error message must name the failure mode; got: {err}"
        );
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "counter must not be touched on wrong-variant error"
        );
    }

    #[test]
    fn handle_dispatch_err_is_propagated_with_path_context() {
        let counter = Arc::new(AtomicU64::new(0));
        let err = voom_domain::errors::VoomError::plugin("discovery", "boom");
        let res = handle_dispatch_result(Err(err), &PathBuf::from("/some/root"), &counter);
        let err = res.expect_err("dispatch Err must propagate");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("filesystem scan failed for /some/root"),
            "error chain must include path context; got: {chain}"
        );
    }
}
