//! Parity tests: kernel-routed orchestration must produce bit-identical
//! OrchestrationResults vs. direct library calls.
//!
//! Uses `serde_json::Value` equality on the full result struct —
//! OrchestrationResult is serde-derived (Phase 1), so this catches any
//! field-level divergence including phase_results, outcomes, skip_reasons,
//! and file_modified.

use std::path::PathBuf;
use std::sync::Arc;

use voom_domain::media::{Container, MediaFile, Track, TrackType};
use voom_dsl::compile_policy;
use voom_kernel::{Kernel, PluginContext};

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
            let mut t = Track::new(1, TrackType::AudioMain, "aac".into());
            t.language = "eng".into();
            t.channels = Some(2);
            t.is_default = true;
            t
        },
    ];
    file
}

fn kernel_with_orchestrator() -> Kernel {
    let mut kernel = Kernel::new();
    let ctx = PluginContext::new(serde_json::json!({}), std::env::temp_dir());
    kernel
        .init_and_register(
            Arc::new(voom_phase_orchestrator::PhaseOrchestratorPlugin::for_bootstrap()),
            37,
            &ctx,
        )
        .expect("register orchestrator");
    kernel
}

fn assert_orchestration_results_equal(
    direct: &voom_phase_orchestrator::OrchestrationResult,
    routed: &voom_phase_orchestrator::OrchestrationResult,
) {
    let direct_v = serde_json::to_value(direct).expect("serialize direct");
    let routed_v = serde_json::to_value(routed).expect("serialize routed");
    assert_eq!(
        direct_v, routed_v,
        "kernel-routed OrchestrationResult differs from direct call:\n  direct: {direct_v}\n  routed: {routed_v}"
    );
}

#[test]
fn parity_simple_single_phase() {
    let kernel = kernel_with_orchestrator();
    let policy =
        compile_policy(r#"policy "demo" { phase init { container mkv } }"#).expect("compile");
    let file = test_file();
    let plans = voom_policy_evaluator::evaluator::evaluate(&policy, &file).plans;

    let direct = voom_phase_orchestrator::orchestrate(plans.clone());

    let routed = voom_cli::kernel_invoke::orchestrate(&kernel, plans, "demo".into())
        .expect("kernel orchestrate");

    assert_orchestration_results_equal(&direct, &routed);
}

#[test]
fn parity_multi_phase_with_skip_and_depends() {
    // Exercise the orchestrate() state machine: dependencies, skip-when, and
    // run-if interact to produce a non-trivial PhaseResult sequence. If the
    // plugin path drops any of those state transitions, the JSON comparison
    // will surface the divergence.
    let kernel = kernel_with_orchestrator();
    let policy = compile_policy(
        r#"policy "demo" {
            phase containerize { container mkv }
            phase normalize {
                depends_on: [containerize]
                keep audio where lang in [eng]
            }
            phase already_hevc {
                depends_on: [normalize]
                skip when video.codec == "hevc"
                transcode video to hevc { crf: 20 }
            }
        }"#,
    )
    .expect("compile");
    let file = test_file(); // hevc — already_hevc skips
    let plans = voom_policy_evaluator::evaluator::evaluate(&policy, &file).plans;

    let direct = voom_phase_orchestrator::orchestrate(plans.clone());

    let routed = voom_cli::kernel_invoke::orchestrate(&kernel, plans, "demo".into())
        .expect("kernel orchestrate");

    assert_orchestration_results_equal(&direct, &routed);

    // Bonus sanity assertion to confirm the test is exercising the skip path —
    // if the policy ever stops triggering the skip branch, the parity test
    // silently becomes weaker.
    assert_eq!(
        routed.phase_results[2].outcome,
        voom_domain::plan::PhaseOutcome::Skipped,
        "already_hevc must be skipped for this parity test to exercise the skip path"
    );
}

#[test]
fn parity_empty_plans() {
    // Edge case: empty plan list. The free function returns an
    // OrchestrationResult with no phase results and file_modified=false;
    // the plugin path must match exactly.
    let kernel = kernel_with_orchestrator();
    let direct = voom_phase_orchestrator::orchestrate(vec![]);
    let routed = voom_cli::kernel_invoke::orchestrate(&kernel, vec![], "empty".into())
        .expect("kernel orchestrate");

    assert_orchestration_results_equal(&direct, &routed);
}
