//! `FFmpeg` plan execution: build commands, run subprocess, manage temp files.

use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;
use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{
    ActionParams, ActionResult, ExecutionDetail, Plan, PlannedAction, SampleStrategy,
};
use voom_domain::transcode::TranscodeOutcome;
use voom_process::run_with_timeout_env;

use voom_domain::temp_file::temp_path_with_ext;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;
use crate::vmaf::FullSample;
use crate::vmaf_iterate::{iterate_to_target, BitrateBounds, IterationError, IterationResult};

/// Default timeout for `FFmpeg` operations (4 hours — transcode can be slow).
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Maximum number of stderr lines to capture in `ExecutionDetail`.
const STDERR_TAIL_LINES: usize = 20;

/// Runs VMAF-guided CRF selection for a transcode action.
pub trait VmafRunner {
    fn run(
        &self,
        source: &std::path::Path,
        target_vmaf: u32,
        bounds: BitrateBounds,
    ) -> std::result::Result<IterationResult, IterationError>;
}

struct DefaultVmafRunner;

pub struct PlanExecution {
    pub action_results: Vec<ActionResult>,
    pub transcode_outcomes: Vec<TranscodeOutcome>,
}

impl VmafRunner for DefaultVmafRunner {
    fn run(
        &self,
        source: &std::path::Path,
        target_vmaf: u32,
        bounds: BitrateBounds,
    ) -> std::result::Result<IterationResult, IterationError> {
        iterate_to_target(source, target_vmaf, bounds, &FullSample, 5)
    }
}

/// Execute a plan by spawning an `FFmpeg` subprocess.
///
/// Builds `FFmpeg` args, runs the command writing to a temp file, then
/// renames the temp file over the original (or to the new extension
/// if converting containers).
pub fn execute_plan(plan: &Plan, hw_accel: &HwAccelConfig) -> Result<Vec<ActionResult>> {
    Ok(execute_plan_with_outcomes(plan, hw_accel)?.action_results)
}

pub fn execute_plan_with_outcomes(plan: &Plan, hw_accel: &HwAccelConfig) -> Result<PlanExecution> {
    execute_plan_with_runner(plan, hw_accel, &DefaultVmafRunner)
}

fn execute_plan_with_runner(
    plan: &Plan,
    hw_accel: &HwAccelConfig,
    vmaf_runner: &dyn VmafRunner,
) -> Result<PlanExecution> {
    if !plan.file.path.exists() {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("file not found: {}", plan.file.path.display()),
        });
    }

    let prepared = prepare_vmaf_transcodes_with_outcomes(plan, vmaf_runner);
    let prepared_plan = prepared.plan;
    let transcode_outcomes = prepared.outcomes;
    let actions: Vec<&PlannedAction> = prepared_plan.actions.iter().collect();
    let ext = output_extension(&prepared_plan.file, &actions);

    // Build the output path (temp file next to original)
    let output_path = temp_path_with_ext(&prepared_plan.file.path, &ext);

    let hw = hw_accel.enabled().then_some(hw_accel);
    let ffmpeg_args = build_ffmpeg_command(&prepared_plan.file, &actions, &output_path, hw)?;

    tracing::info!(
        path = %prepared_plan.file.path.display(),
        phase = %prepared_plan.phase_name,
        actions = actions.len(),
        output = %output_path.display(),
        "executing ffmpeg"
    );
    tracing::debug!(args = ?ffmpeg_args, "ffmpeg command");

    let command_str = voom_process::shell_quote_args("ffmpeg", &ffmpeg_args);
    let env_vars: Vec<(&str, &str)> = hw_accel.device_env().into_iter().collect();
    let start = Instant::now();
    let output = run_with_timeout_env("ffmpeg", &ffmpeg_args, FFMPEG_TIMEOUT, &env_vars);
    let duration_ms = start.elapsed().as_millis() as u64;

    match output {
        Ok(output) if output.status.success() => {
            let final_path = rename_output(&prepared_plan, &output_path, &ext)?;

            tracing::info!(
                path = %final_path.display(),
                actions = actions.len(),
                "ffmpeg execution complete"
            );

            let detail = ExecutionDetail {
                command: command_str,
                exit_code: Some(0),
                stderr_tail: String::new(),
                duration_ms,
            };
            let action_results = actions
                .iter()
                .map(|a| {
                    ActionResult::success(a.operation, a.description.clone())
                        .with_execution_detail(detail.clone())
                })
                .collect();
            Ok(PlanExecution {
                action_results,
                transcode_outcomes,
            })
        }
        Ok(output) => {
            let _ = std::fs::remove_file(&output_path);
            tracing::debug!(
                args = ?ffmpeg_args,
                "ffmpeg failed"
            );
            let tail = voom_process::stderr_tail(&output.stderr, STDERR_TAIL_LINES);
            let display_tail = if tail.is_empty() {
                "(no output)"
            } else {
                &tail
            };
            let error_msg = format!(
                "ffmpeg exited with {}:\n{}\ncmd: {}",
                output.status, display_tail, command_str
            );
            let detail = ExecutionDetail {
                command: command_str,
                exit_code: output.status.code(),
                stderr_tail: tail,
                duration_ms,
            };
            let action_results = vec![ActionResult::failure(
                actions[0].operation,
                &actions[0].description,
                &error_msg,
            )
            .with_execution_detail(detail)];
            Ok(PlanExecution {
                action_results,
                transcode_outcomes,
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&output_path);
            Err(e)
        }
    }
}

pub(crate) struct PreparedTranscodes {
    pub plan: Plan,
    pub outcomes: Vec<TranscodeOutcome>,
}

pub(crate) fn prepare_vmaf_transcodes_with_outcomes(
    plan: &Plan,
    runner: &dyn VmafRunner,
) -> PreparedTranscodes {
    let mut prepared = plan.clone();
    let mut outcomes = Vec::new();
    for action in &mut prepared.actions {
        let stats = prepare_vmaf_action(&plan.file.path, action, runner);
        if let Some(outcome) = transcode_outcome(&plan.file.id.to_string(), action, stats) {
            outcomes.push(outcome);
        }
    }
    PreparedTranscodes {
        plan: prepared,
        outcomes,
    }
}

#[cfg(test)]
fn prepare_vmaf_transcodes(plan: &Plan, runner: &dyn VmafRunner) -> Plan {
    prepare_vmaf_transcodes_with_outcomes(plan, runner).plan
}

fn prepare_vmaf_action(
    source: &std::path::Path,
    action: &mut PlannedAction,
    runner: &dyn VmafRunner,
) -> OutcomeStats {
    let ActionParams::Transcode { settings, .. } = &mut action.parameters else {
        return OutcomeStats::default();
    };
    let Some(target_vmaf) = settings.target_vmaf else {
        return OutcomeStats::default();
    };
    let bounds = BitrateBounds {
        min_bitrate: settings.min_bitrate.clone(),
        max_bitrate: settings.max_bitrate.clone(),
    };
    match runner.run(source, target_vmaf, bounds) {
        Ok(result) => {
            settings.crf = Some(result.final_crf);
            settings.bitrate = result.final_bitrate;
            settings.preset = None;
            OutcomeStats {
                achieved_vmaf: Some(result.achieved_vmaf as f32),
                iterations: result.iterations,
                fallback_used: false,
            }
        }
        Err(error) => {
            tracing::warn!(%error, "VMAF iteration failed; using transcode fallback");
            if let Some(fallback) = &settings.fallback {
                settings.crf = Some(fallback.crf);
                settings.preset = Some(fallback.preset.clone());
            }
            OutcomeStats {
                achieved_vmaf: None,
                iterations: 1,
                fallback_used: true,
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OutcomeStats {
    achieved_vmaf: Option<f32>,
    iterations: u32,
    fallback_used: bool,
}

impl Default for OutcomeStats {
    fn default() -> Self {
        Self {
            achieved_vmaf: None,
            iterations: 1,
            fallback_used: false,
        }
    }
}

fn transcode_outcome(
    file_id: &str,
    action: &PlannedAction,
    stats: OutcomeStats,
) -> Option<TranscodeOutcome> {
    let ActionParams::Transcode { settings, .. } = &action.parameters else {
        return None;
    };
    let sample_strategy = settings
        .sample_strategy
        .clone()
        .unwrap_or(SampleStrategy::Full);
    Some(TranscodeOutcome {
        id: Uuid::new_v4(),
        file_id: file_id.to_string(),
        target_vmaf: settings.target_vmaf,
        achieved_vmaf: stats.achieved_vmaf,
        crf_used: settings.crf,
        bitrate_used: settings.bitrate.clone(),
        iterations: stats.iterations,
        sample_strategy,
        fallback_used: stats.fallback_used,
        completed_at: Utc::now(),
    })
}

/// Rename the temp output file to its final location.
///
/// If the extension changed (container conversion), rename to the new extension
/// and remove the old file. Otherwise, rename over the original.
fn rename_output(
    plan: &Plan,
    output_path: &std::path::Path,
    ext: &str,
) -> Result<std::path::PathBuf> {
    let original_ext = plan
        .file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if ext == original_ext {
        // Same extension: rename temp over original
        std::fs::rename(output_path, &plan.file.path).map_err(|e| {
            let _ = std::fs::remove_file(output_path);
            VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!(
                    "failed to rename temp file to {}: {e}",
                    plan.file.path.display()
                ),
            }
        })?;
        Ok(plan.file.path.clone())
    } else {
        // Container conversion: rename to new extension
        let new_path = plan.file.path.with_extension(ext);
        std::fs::rename(output_path, &new_path).map_err(|e| {
            let _ = std::fs::remove_file(output_path);
            VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!("failed to rename temp file to {}: {e}", new_path.display()),
            }
        })?;
        // Remove old file if extension changed
        if new_path != plan.file.path {
            let _ = std::fs::remove_file(&plan.file.path);
        }
        Ok(new_path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::{
        ActionParams, OperationType, Plan, PlannedAction, TranscodeFallback, TranscodeSettings,
    };

    use super::*;
    use crate::vmaf_iterate::{BitrateBounds, IterationError, IterationResult};

    struct FailingVmafRunner;
    struct SuccessfulVmafRunner;

    impl VmafRunner for FailingVmafRunner {
        fn run(
            &self,
            _source: &Path,
            _target_vmaf: u32,
            _bounds: BitrateBounds,
        ) -> std::result::Result<IterationResult, IterationError> {
            Err(IterationError::InvalidInput(
                "libvmaf unavailable".to_string(),
            ))
        }
    }

    impl VmafRunner for SuccessfulVmafRunner {
        fn run(
            &self,
            _source: &Path,
            _target_vmaf: u32,
            _bounds: BitrateBounds,
        ) -> std::result::Result<IterationResult, IterationError> {
            Ok(IterationResult {
                final_crf: 18,
                final_bitrate: Some("5200k".to_string()),
                achieved_vmaf: 92.3,
                iterations: 3,
            })
        }
    }

    fn video_plan(settings: TranscodeSettings) -> Plan {
        let mut file = MediaFile::new(PathBuf::from("/media/video.mp4"));
        file.container = Container::Mp4;
        file.tracks = vec![Track::new(0, TrackType::Video, "h264".into())];
        Plan::new(file, "policy", "phase").with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".to_string(),
                settings,
            },
            "transcode video",
        ))
    }

    #[test]
    fn prepare_vmaf_transcodes_uses_fallback_when_iteration_fails() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(92))
            .with_fallback(Some(TranscodeFallback::new(24, "medium")));
        let plan = video_plan(settings);

        let prepared = prepare_vmaf_transcodes(&plan, &FailingVmafRunner);

        let ActionParams::Transcode { settings, .. } = &prepared.actions[0].parameters else {
            panic!("expected transcode parameters");
        };
        assert_eq!(settings.crf, Some(24));
        assert_eq!(settings.preset.as_deref(), Some("medium"));
        assert_eq!(settings.target_vmaf, Some(92));
    }

    #[test]
    fn prepare_vmaf_transcodes_uses_converged_iteration_settings() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(92))
            .with_fallback(Some(TranscodeFallback::new(24, "medium")));
        let plan = video_plan(settings);

        let prepared = prepare_vmaf_transcodes(&plan, &SuccessfulVmafRunner);

        let ActionParams::Transcode { settings, .. } = &prepared.actions[0].parameters else {
            panic!("expected transcode parameters");
        };
        assert_eq!(settings.crf, Some(18));
        assert_eq!(settings.bitrate.as_deref(), Some("5200k"));
        assert_eq!(settings.preset, None);
    }

    #[test]
    fn prepare_vmaf_transcodes_records_success_outcome() {
        let settings = TranscodeSettings::default().with_target_vmaf(Some(92));
        let plan = video_plan(settings);

        let prepared = prepare_vmaf_transcodes_with_outcomes(&plan, &SuccessfulVmafRunner);

        assert_eq!(prepared.outcomes.len(), 1);
        let outcome = &prepared.outcomes[0];
        assert_eq!(outcome.file_id, plan.file.id.to_string());
        assert_eq!(outcome.target_vmaf, Some(92));
        assert_eq!(outcome.achieved_vmaf, Some(92.3));
        assert_eq!(outcome.crf_used, Some(18));
        assert_eq!(outcome.bitrate_used.as_deref(), Some("5200k"));
        assert_eq!(outcome.iterations, 3);
        assert!(!outcome.fallback_used);
    }

    #[test]
    fn prepare_vmaf_transcodes_records_non_target_outcome() {
        let settings = TranscodeSettings::default().with_crf(Some(23));
        let plan = video_plan(settings);

        let prepared = prepare_vmaf_transcodes_with_outcomes(&plan, &SuccessfulVmafRunner);

        assert_eq!(prepared.outcomes.len(), 1);
        let outcome = &prepared.outcomes[0];
        assert_eq!(outcome.target_vmaf, None);
        assert_eq!(outcome.achieved_vmaf, None);
        assert_eq!(outcome.crf_used, Some(23));
        assert_eq!(outcome.iterations, 1);
        assert!(!outcome.fallback_used);
    }

    #[test]
    fn prepare_vmaf_transcodes_records_fallback_outcome() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(92))
            .with_fallback(Some(TranscodeFallback::new(24, "medium")));
        let plan = video_plan(settings);

        let prepared = prepare_vmaf_transcodes_with_outcomes(&plan, &FailingVmafRunner);

        assert_eq!(prepared.outcomes.len(), 1);
        let outcome = &prepared.outcomes[0];
        assert_eq!(outcome.target_vmaf, Some(92));
        assert_eq!(outcome.achieved_vmaf, None);
        assert_eq!(outcome.crf_used, Some(24));
        assert_eq!(outcome.iterations, 1);
        assert!(outcome.fallback_used);
    }
}
