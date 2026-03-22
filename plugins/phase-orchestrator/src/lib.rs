//! Phase Orchestrator Plugin.
//!
//! Produces an [`OrchestrationResult`] from pre-evaluated plans: determines
//! phase outcomes based on `skip_when`, `run_if`, and `depends_on` results,
//! and provides dry-run formatting. Does not call executors — the CLI's
//! `process` command handles actual execution and re-introspection.

#![allow(clippy::missing_errors_doc)]

use voom_domain::capabilities::Capability;
use voom_domain::compiled::{CompiledPolicy, ErrorStrategy};
use voom_domain::errors::Result;
use voom_domain::plan::{PhaseOutcome, PhaseResult, Plan};
use voom_kernel::Plugin;

/// Result of orchestrating all phases of a policy.
#[non_exhaustive]
#[derive(Debug)]
pub struct OrchestrationResult {
    /// Plans produced for each phase (in execution order).
    pub plans: Vec<Plan>,
    /// Results for each executed phase.
    pub phase_results: Vec<PhaseResult>,
    /// Whether any phase modified the file.
    pub file_modified: bool,
}

impl OrchestrationResult {
    /// Create a new orchestration result.
    #[must_use]
    pub fn new(plans: Vec<Plan>, phase_results: Vec<PhaseResult>, file_modified: bool) -> Self {
        Self {
            plans,
            phase_results,
            file_modified,
        }
    }
}

/// The phase orchestrator plugin.
///
/// Manages the execution order and dependencies between phases. In a full
/// pipeline, it coordinates: evaluate → execute → re-introspect → next phase.
pub struct PhaseOrchestratorPlugin {
    capabilities: Vec<Capability>,
}

impl PhaseOrchestratorPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::Orchestrate],
        }
    }

    /// Orchestrate evaluation of all phases in a policy against a file.
    ///
    /// This is a "dry run" orchestration that produces plans without executing them.
    /// In a full pipeline, the orchestrator would also invoke executors between phases
    /// and re-introspect the file after modifications.
    ///
    /// The caller is responsible for running policy evaluation first (via
    /// `voom_policy_evaluator::evaluator::evaluate`) and passing the resulting plans.
    pub fn orchestrate(&self, plans: Vec<Plan>) -> Result<OrchestrationResult> {
        let mut phase_results = Vec::new();
        let mut file_modified = false;

        for plan in &plans {
            let outcome = if plan.is_skipped() {
                PhaseOutcome::Skipped
            } else if plan.actions.is_empty() {
                PhaseOutcome::Completed
            } else {
                file_modified = true;
                PhaseOutcome::Pending // Would be Completed after execution
            };

            let mut phase_result = PhaseResult::new(plan.phase_name.clone(), outcome);
            phase_result.file_modified = !plan.actions.is_empty();
            phase_result.skip_reason = plan.skip_reason.clone();
            phase_results.push(phase_result);
        }

        Ok(OrchestrationResult {
            plans,
            phase_results,
            file_modified,
        })
    }

    /// Build a human-readable dry-run summary.
    #[must_use]
    pub fn format_dry_run(result: &OrchestrationResult) -> String {
        let mut output = String::new();

        for (plan, phase_result) in result.plans.iter().zip(&result.phase_results) {
            output.push_str(&format!("\n=== Phase: {} ===\n", plan.phase_name));

            if let Some(ref reason) = phase_result.skip_reason {
                output.push_str(&format!("  SKIPPED: {reason}\n"));
                continue;
            }

            if plan.actions.is_empty() {
                output.push_str("  No actions needed.\n");
            } else {
                for (i, action) in plan.actions.iter().enumerate() {
                    output.push_str(&format!("  {}. {}\n", i + 1, action.description));
                }
            }

            for warning in &plan.warnings {
                output.push_str(&format!("  WARNING: {warning}\n"));
            }
        }

        output
    }

    /// Determine if the entire policy requires file modifications.
    #[must_use]
    pub fn needs_execution(result: &OrchestrationResult) -> bool {
        result
            .plans
            .iter()
            .any(|p| !p.is_skipped() && !p.is_empty())
    }

    /// Get the error strategy for a given phase.
    #[must_use]
    pub fn phase_error_strategy(policy: &CompiledPolicy, phase_name: &str) -> ErrorStrategy {
        policy
            .phases
            .iter()
            .find(|p| p.name == phase_name)
            .map(|p| p.on_error)
            // The compiler always sets an explicit on_error per phase, so this
            // is only reachable if called with a phase name not in the policy.
            .unwrap_or(ErrorStrategy::Abort)
    }
}

impl Default for PhaseOrchestratorPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for PhaseOrchestratorPlugin {
    fn name(&self) -> &str {
        "phase-orchestrator"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::events::Event;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

    fn eval(policy: &CompiledPolicy, file: &MediaFile) -> Vec<Plan> {
        voom_policy_evaluator::evaluator::evaluate(policy, file).plans
    }

    fn test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/media/Movie.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![
            {
                let mut t = Track::new(0, TrackType::Video, "hevc".into());
                t.width = Some(1920);
                t.height = Some(1080);
                t
            },
            {
                let mut t = Track::new(1, TrackType::AudioMain, "dts_hd".into());
                t.language = "eng".into();
                t.channels = Some(8);
                t.is_default = true;
                t
            },
            {
                let mut t = Track::new(2, TrackType::AudioAlternate, "aac".into());
                t.language = "jpn".into();
                t.channels = Some(2);
                t
            },
            {
                let mut t = Track::new(3, TrackType::SubtitleMain, "srt".into());
                t.language = "eng".into();
                t
            },
        ];
        file
    }

    #[test]
    fn test_orchestrate_simple_policy() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert_eq!(result.plans.len(), 1);
        assert!(!result.file_modified); // Already MKV
    }

    #[test]
    fn test_orchestrate_multi_phase() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize { container mkv }
                phase normalize {
                    depends_on: [containerize]
                    keep audio where lang in [eng]
                }
            }"#,
        )
        .unwrap();
        let file = test_file();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert_eq!(result.plans.len(), 2);
        // normalize phase should remove jpn audio
        assert!(result.file_modified);
    }

    #[test]
    fn test_orchestrate_skipped_phases() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase tc {
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
            }"#,
        )
        .unwrap();
        let file = test_file(); // video is hevc
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert!(result.plans[0].is_skipped());
        assert!(!result.file_modified);
    }

    #[test]
    fn test_format_dry_run() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                config { on_error: continue }
                phase containerize { container mkv }
                phase normalize {
                    depends_on: [containerize]
                    keep audio where lang in [eng]
                }
            }"#,
        )
        .unwrap();
        let file = test_file();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        let output = PhaseOrchestratorPlugin::format_dry_run(&result);
        assert!(output.contains("Phase: containerize"));
        assert!(output.contains("Phase: normalize"));
        assert!(output.contains("Remove audio track"));
    }

    #[test]
    fn test_needs_execution() {
        let orch = PhaseOrchestratorPlugin::new();

        // No changes needed
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert!(!PhaseOrchestratorPlugin::needs_execution(&result));

        // Changes needed
        let policy = voom_dsl::compile_policy(
            r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#,
        )
        .unwrap();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert!(PhaseOrchestratorPlugin::needs_execution(&result));
    }

    #[test]
    fn test_phase_error_strategy() {
        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                config { on_error: continue }
                phase a {
                    on_error: skip
                    container mkv
                }
                phase b { container mkv }
            }"#,
        )
        .unwrap();
        assert_eq!(
            PhaseOrchestratorPlugin::phase_error_strategy(&policy, "a"),
            ErrorStrategy::Skip
        );
        // Phase b has no explicit on_error, so compiler defaults to Abort
        assert_eq!(
            PhaseOrchestratorPlugin::phase_error_strategy(&policy, "b"),
            ErrorStrategy::Abort
        );
    }

    #[test]
    fn test_orchestrate_run_if() {
        let orch = PhaseOrchestratorPlugin::new();
        let file = test_file(); // already MKV

        let policy = voom_dsl::compile_policy(
            r#"policy "test" {
                phase containerize { container mkv }
                phase validate {
                    depends_on: [containerize]
                    run_if containerize.modified
                    when exists(audio where lang == eng) { warn "has eng" }
                }
            }"#,
        )
        .unwrap();

        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert_eq!(result.plans.len(), 2);
        // containerize does nothing (already MKV), so validate is skipped
        assert!(result.plans[1].is_skipped());
    }

    #[test]
    fn test_orchestrate_production_policy() {
        let orch = PhaseOrchestratorPlugin::new();
        let source =
            include_str!("../../../crates/voom-dsl/tests/fixtures/production-normalize.voom");
        let policy = voom_dsl::compile_policy(source).unwrap();
        let file = test_file();
        let result = orch.orchestrate(eval(&policy, &file)).unwrap();
        assert_eq!(result.plans.len(), 6);

        let output = PhaseOrchestratorPlugin::format_dry_run(&result);
        assert!(!output.is_empty());
    }

    #[test]
    fn test_plugin_trait_impl() {
        let orch = PhaseOrchestratorPlugin::new();
        assert_eq!(orch.name(), "phase-orchestrator");
        // Orchestration is driven via direct API, not through event bus
        assert!(!orch.handles(Event::FILE_DISCOVERED));
        assert_eq!(orch.capabilities().len(), 1);
    }
}
