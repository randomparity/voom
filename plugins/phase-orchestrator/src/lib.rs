//! Phase Orchestrator Plugin.
//!
//! Produces an [`OrchestrationResult`] from pre-evaluated plans: determines
//! phase outcomes based on `skip_when`, `run_if`, and `depends_on` results,
//! and provides dry-run formatting. Does not call executors — the CLI's
//! `process` command handles actual execution and re-introspection.

pub use voom_domain::orchestration::OrchestrationResult;
use voom_domain::plan::{PhaseOutcome, PhaseResult, Plan};
use voom_dsl::compiled::{CompiledPolicy, ErrorStrategy};

pub mod plugin;

pub use plugin::PhaseOrchestratorPlugin;

/// Produce phase outcomes from pre-evaluated plans, computing skip/completion
/// state and dry-run summary.
///
/// The caller is responsible for running policy evaluation first (via
/// `voom_policy_evaluator::evaluator::evaluate`) and passing the resulting plans.
#[must_use]
pub fn orchestrate(plans: Vec<Plan>) -> OrchestrationResult {
    let mut phase_results = Vec::new();
    let mut file_modified = false;

    for plan in &plans {
        let outcome = if plan.is_skipped() {
            PhaseOutcome::Skipped
        } else if plan.actions.is_empty() {
            PhaseOutcome::Completed
        } else {
            PhaseOutcome::Pending // Would be Completed after execution
        };

        let phase_modified = outcome == PhaseOutcome::Pending;
        if phase_modified {
            file_modified = true;
        }

        let mut phase_result = PhaseResult::new(plan.phase_name.clone(), outcome);
        phase_result.file_modified = phase_modified;
        phase_result.skip_reason.clone_from(&plan.skip_reason);
        phase_results.push(phase_result);
    }

    OrchestrationResult::new(plans, phase_results, file_modified)
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
        .map_or(ErrorStrategy::Abort, |p| p.on_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orchestrate(eval(&policy, &file));
        assert_eq!(result.plans.len(), 1);
        assert!(!result.file_modified); // Already MKV
    }

    #[test]
    fn test_orchestrate_multi_phase() {
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
        let result = orchestrate(eval(&policy, &file));
        assert_eq!(result.plans.len(), 2);
        // normalize phase should remove jpn audio
        assert!(result.file_modified);
    }

    #[test]
    fn test_orchestrate_skipped_phases() {
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
        let result = orchestrate(eval(&policy, &file));
        assert!(result.plans[0].is_skipped());
        assert!(!result.file_modified);
    }

    #[test]
    fn test_format_dry_run() {
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
        let result = orchestrate(eval(&policy, &file));
        let output = format_dry_run(&result);
        assert!(output.contains("Phase: containerize"));
        assert!(output.contains("Phase: normalize"));
        assert!(output.contains("Remove audio track"));
    }

    #[test]
    fn test_needs_execution() {
        // No changes needed
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = test_file();
        let result = orchestrate(eval(&policy, &file));
        assert!(!needs_execution(&result));

        // Changes needed
        let policy = voom_dsl::compile_policy(
            r#"policy "test" { phase norm { keep audio where lang in [eng] } }"#,
        )
        .unwrap();
        let result = orchestrate(eval(&policy, &file));
        assert!(needs_execution(&result));
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
        assert_eq!(phase_error_strategy(&policy, "a"), ErrorStrategy::Skip);
        // Phase b has no explicit on_error, so compiler defaults to Abort
        assert_eq!(phase_error_strategy(&policy, "b"), ErrorStrategy::Abort);
    }

    #[test]
    fn test_orchestrate_run_if() {
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

        let result = orchestrate(eval(&policy, &file));
        assert_eq!(result.plans.len(), 2);
        // containerize does nothing (already MKV), so validate is skipped
        assert!(result.plans[1].is_skipped());
    }

    #[test]
    fn test_orchestrate_production_policy() {
        let source =
            include_str!("../../../crates/voom-dsl/tests/fixtures/production-normalize.voom");
        let policy = voom_dsl::compile_policy(source).unwrap();
        let file = test_file();
        let result = orchestrate(eval(&policy, &file));
        assert_eq!(result.plans.len(), 6);

        let output = format_dry_run(&result);
        assert!(!output.is_empty());
    }
}
