//! Phase Orchestrator Plugin.
//!
//! Sequences phase execution with dependency resolution, `skip_when` evaluation,
//! `run_if` triggers, and per-phase error handling. Coordinates the policy
//! evaluator and executors to process files through all phases of a policy.

#![allow(clippy::missing_errors_doc)]

use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_domain::media::MediaFile;
use voom_domain::plan::{PhaseOutcome, PhaseResult, Plan};
use voom_dsl::compiler::{CompiledPolicy, ErrorStrategy};
use voom_kernel::{Plugin, PluginContext};

/// Result of orchestrating all phases of a policy.
#[derive(Debug)]
pub struct OrchestrationResult {
    /// Plans produced for each phase (in execution order).
    pub plans: Vec<Plan>,
    /// Results for each executed phase.
    pub phase_results: Vec<PhaseResult>,
    /// Whether any phase modified the file.
    pub file_modified: bool,
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
    pub fn orchestrate(
        &self,
        policy: &CompiledPolicy,
        file: &MediaFile,
    ) -> Result<OrchestrationResult> {
        let eval_result = voom_policy_evaluator::evaluator::evaluate(policy, file);

        let mut phase_results = Vec::new();
        let mut file_modified = false;

        for plan in &eval_result.plans {
            let outcome = if plan.is_skipped() {
                PhaseOutcome::Skipped
            } else if plan.actions.is_empty() {
                PhaseOutcome::Completed
            } else {
                file_modified = true;
                PhaseOutcome::Pending // Would be Completed after execution
            };

            phase_results.push(PhaseResult {
                phase_name: plan.phase_name.clone(),
                outcome,
                actions: Vec::new(), // Populated after execution
                file_modified: !plan.actions.is_empty(),
                skip_reason: plan.skip_reason.clone(),
                duration_ms: 0,
            });
        }

        Ok(OrchestrationResult {
            plans: eval_result.plans,
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
            .unwrap_or(policy.config.on_error)
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

    fn handles(&self, _event_type: &str) -> bool {
        // Phase orchestration is triggered via direct API call (orchestrate()),
        // not through the event bus. The CLI's process command calls orchestrate()
        // directly for deterministic progress reporting and concurrency control.
        false
    }

    fn on_event(&self, _event: &Event) -> Result<Option<EventResult>> {
        Ok(None)
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{Container, MediaFile, Track, TrackType};

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
            voom_dsl::compile(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert_eq!(result.plans.len(), 1);
        assert!(!result.file_modified); // Already MKV
    }

    #[test]
    fn test_orchestrate_multi_phase() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile(
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
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert_eq!(result.plans.len(), 2);
        // normalize phase should remove jpn audio
        assert!(result.file_modified);
    }

    #[test]
    fn test_orchestrate_skipped_phases() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile(
            r#"policy "test" {
                phase tc {
                    skip when video.codec == "hevc"
                    transcode video to hevc { crf: 20 }
                }
            }"#,
        )
        .unwrap();
        let file = test_file(); // video is hevc
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert!(result.plans[0].is_skipped());
        assert!(!result.file_modified);
    }

    #[test]
    fn test_format_dry_run() {
        let orch = PhaseOrchestratorPlugin::new();
        let policy = voom_dsl::compile(
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
        let result = orch.orchestrate(&policy, &file).unwrap();
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
            voom_dsl::compile(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert!(!PhaseOrchestratorPlugin::needs_execution(&result));

        // Changes needed
        let policy =
            voom_dsl::compile(r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#)
                .unwrap();
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert!(PhaseOrchestratorPlugin::needs_execution(&result));
    }

    #[test]
    fn test_phase_error_strategy() {
        let policy = voom_dsl::compile(
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

        let policy = voom_dsl::compile(
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

        let result = orch.orchestrate(&policy, &file).unwrap();
        assert_eq!(result.plans.len(), 2);
        // containerize does nothing (already MKV), so validate is skipped
        assert!(result.plans[1].is_skipped());
    }

    #[test]
    fn test_orchestrate_production_policy() {
        let orch = PhaseOrchestratorPlugin::new();
        let source =
            include_str!("../../../crates/voom-dsl/tests/fixtures/production-normalize.voom");
        let policy = voom_dsl::compile(source).unwrap();
        let file = test_file();
        let result = orch.orchestrate(&policy, &file).unwrap();
        assert_eq!(result.plans.len(), 6);

        let output = PhaseOrchestratorPlugin::format_dry_run(&result);
        assert!(!output.is_empty());
    }

    #[test]
    fn test_plugin_trait_impl() {
        let orch = PhaseOrchestratorPlugin::new();
        assert_eq!(orch.name(), "phase-orchestrator");
        // Orchestration is driven via direct API, not through event bus
        assert!(!orch.handles(Event::POLICY_EVALUATE));
        assert!(!orch.handles(Event::FILE_DISCOVERED));
        assert_eq!(orch.capabilities().len(), 1);
    }
}
