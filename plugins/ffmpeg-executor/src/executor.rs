//! `FFmpeg` plan execution: build commands, run subprocess, manage temp files.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use chrono::Utc;
use uuid::Uuid;
use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{
    ActionParams, ActionResult, ExecutionDetail, LoudnessMeasurement, Plan, PlannedAction,
    SampleStrategy,
};
use voom_domain::scan_session_mutations::record_mutation_for_pending_write;
use voom_domain::storage::ScanSessionMutationStorage;
use voom_domain::transcode::TranscodeOutcome;
use voom_domain::transition::ScanSessionId;
use voom_process::run_with_timeout_env;

use voom_domain::temp_file::temp_path_with_ext;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;
use crate::vmaf::FullSample;
use crate::vmaf_iterate::{BitrateBounds, IterationError, IterationResult, iterate_to_target};

/// Default timeout for `FFmpeg` operations (4 hours — transcode can be slow).
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Maximum number of stderr lines to capture in `ExecutionDetail`.
const STDERR_TAIL_LINES: usize = 20;

const TOOL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

trait ProcessRunner {
    fn run(
        &self,
        tool: &str,
        args: &[String],
        timeout: Duration,
        env_vars: &[(&str, &str)],
    ) -> Result<Output>;
}

struct DefaultProcessRunner;

impl ProcessRunner for DefaultProcessRunner {
    fn run(
        &self,
        tool: &str,
        args: &[String],
        timeout: Duration,
        env_vars: &[(&str, &str)],
    ) -> Result<Output> {
        run_with_timeout_env(tool, args, timeout, env_vars)
    }
}

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
    pub loudness_updates: Vec<LoudnessTrackUpdate>,
}

#[derive(Debug, Clone)]
pub struct LoudnessTrackUpdate {
    pub track_index: u32,
    pub integrated_lufs: f64,
    pub true_peak_db: f64,
    pub loudness_range_lu: Option<f64>,
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
    Ok(execute_plan_with_outcomes(plan, hw_accel, None)?.action_results)
}

pub fn execute_plan_with_outcomes(
    plan: &Plan,
    hw_accel: &HwAccelConfig,
    storage: Option<&dyn ScanSessionMutationStorage>,
) -> Result<PlanExecution> {
    execute_plan_with_runner(plan, hw_accel, &DefaultVmafRunner, storage)
}

fn execute_plan_with_runner(
    plan: &Plan,
    hw_accel: &HwAccelConfig,
    vmaf_runner: &dyn VmafRunner,
    storage: Option<&dyn ScanSessionMutationStorage>,
) -> Result<PlanExecution> {
    execute_plan_with_runners(plan, hw_accel, vmaf_runner, &DefaultProcessRunner, storage)
}

fn execute_plan_with_runners(
    plan: &Plan,
    hw_accel: &HwAccelConfig,
    vmaf_runner: &dyn VmafRunner,
    process_runner: &dyn ProcessRunner,
    storage: Option<&dyn ScanSessionMutationStorage>,
) -> Result<PlanExecution> {
    if !plan.file.path.exists() {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("file not found: {}", plan.file.path.display()),
        });
    }

    let prepared = prepare_vmaf_transcodes_with_outcomes(plan, vmaf_runner);
    let mut prepared_plan = prepared.plan;
    let loudness_updates = prepare_loudness_actions(&mut prepared_plan, hw_accel)?;
    let transcode_outcomes = prepared.outcomes;
    let actions: Vec<&PlannedAction> = prepared_plan.actions.iter().collect();
    let ext = output_extension(&prepared_plan.file, &actions);

    // Build the output path (temp file next to original)
    let output_path = temp_path_with_ext(&prepared_plan.file.path, &ext);

    let hw = hw_accel.enabled().then_some(hw_accel);
    let ffmpeg_args = build_ffmpeg_command(&prepared_plan.file, &actions, &output_path, hw)?;
    let dynamic_hdr = dynamic_hdr_job(&prepared_plan, &actions, &ext)?;
    let env_vars: Vec<(&str, &str)> = hw_accel.device_env().into_iter().collect();
    if let Some(job) = &dynamic_hdr {
        ensure_tool_available(job.tool_name(), process_runner, &env_vars)?;
    }

    tracing::info!(
        path = %prepared_plan.file.path.display(),
        phase = %prepared_plan.phase_name,
        actions = actions.len(),
        output = %output_path.display(),
        "executing ffmpeg"
    );
    tracing::debug!(args = ?ffmpeg_args, "ffmpeg command");

    let command_str = voom_process::shell_quote_args("ffmpeg", &ffmpeg_args);
    let start = Instant::now();
    let output = process_runner.run("ffmpeg", &ffmpeg_args, FFMPEG_TIMEOUT, &env_vars);
    let duration_ms = start.elapsed().as_millis() as u64;

    match output {
        Ok(output) if output.status.success() => {
            if let Some(job) = &dynamic_hdr {
                if let Err(error) = apply_dynamic_hdr_reinjection(
                    job,
                    &prepared_plan.file.path,
                    &output_path,
                    &env_vars,
                    process_runner,
                    storage,
                    prepared_plan.scan_session,
                ) {
                    let _ = std::fs::remove_file(&output_path);
                    return Err(error);
                }
            }
            let final_path = rename_output(&prepared_plan, &output_path, &ext, storage)?;

            tracing::info!(
                path = %final_path.display(),
                actions = actions.len(),
                "ffmpeg execution complete"
            );

            let detail = ExecutionDetail {
                command: command_str,
                exit_code: Some(0),
                stderr_tail: String::new(),
                stderr_full: None,
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
                loudness_updates,
            })
        }
        Ok(output) => {
            let _ = std::fs::remove_file(&output_path);
            tracing::debug!(
                args = ?ffmpeg_args,
                "ffmpeg failed"
            );
            let tail = voom_process::stderr_tail(&output.stderr, STDERR_TAIL_LINES);
            let stderr_full = crate::capture_stderr_full(&output.stderr);
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
                stderr_full: Some(stderr_full),
                duration_ms,
            };
            let action_results = vec![
                ActionResult::failure(actions[0].operation, &actions[0].description, &error_msg)
                    .with_execution_detail(detail),
            ];
            Ok(PlanExecution {
                action_results,
                transcode_outcomes,
                loudness_updates: Vec::new(),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&output_path);
            Err(e)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DynamicHdrKind {
    Hdr10Plus,
    DolbyVision,
}

impl DynamicHdrKind {
    fn tool_name(self) -> &'static str {
        match self {
            Self::Hdr10Plus => "hdr10plus_tool",
            Self::DolbyVision => "dovi_tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DynamicHdrJob {
    kind: DynamicHdrKind,
    source_track_index: u32,
    output_extension: String,
}

impl DynamicHdrJob {
    fn tool_name(&self) -> &'static str {
        self.kind.tool_name()
    }
}

fn dynamic_hdr_job(
    plan: &Plan,
    actions: &[&PlannedAction],
    output_extension: &str,
) -> Result<Option<DynamicHdrJob>> {
    let Some(action) = actions
        .iter()
        .find(|action| action.operation == voom_domain::plan::OperationType::TranscodeVideo)
    else {
        return Ok(None);
    };
    let ActionParams::Transcode { codec, settings } = &action.parameters else {
        return Ok(None);
    };
    let Some(track_index) = action.track_index else {
        return Ok(None);
    };
    let Some(track) = plan
        .file
        .tracks
        .iter()
        .find(|track| track.index == track_index)
    else {
        return Ok(None);
    };
    if should_tonemap(settings) || !settings.preserve_hdr.unwrap_or(true) {
        return Ok(None);
    }
    let Some(kind) = dynamic_hdr_kind(track)? else {
        return Ok(None);
    };
    if !matches!(codec.as_str(), "hevc" | "h265") {
        return Err(dynamic_hdr_error(format!(
            "cannot preserve dynamic HDR metadata while transcoding to {codec}; \
             choose hevc/h265 or set preserve_hdr: false"
        )));
    }
    if !matches!(output_extension, "mkv" | "mp4") {
        return Err(dynamic_hdr_error(format!(
            "cannot preserve dynamic HDR metadata in .{output_extension} output; \
             choose mkv or mp4, or set preserve_hdr: false"
        )));
    }
    Ok(Some(DynamicHdrJob {
        kind,
        source_track_index: track.index,
        output_extension: output_extension.to_string(),
    }))
}

fn dynamic_hdr_kind(track: &voom_domain::media::Track) -> Result<Option<DynamicHdrKind>> {
    let format = track.hdr_format.as_deref().unwrap_or_default();
    if format.contains("HDR10+") {
        return Ok(Some(DynamicHdrKind::Hdr10Plus));
    }
    if format.contains("Dolby Vision") {
        let Some(profile) = track.dolby_vision_profile else {
            return Err(dynamic_hdr_error(
                "cannot preserve Dolby Vision RPU metadata without a detected profile",
            ));
        };
        if matches!(profile, 5 | 7 | 8) {
            return Ok(Some(DynamicHdrKind::DolbyVision));
        }
        return Err(dynamic_hdr_error(format!(
            "unsupported Dolby Vision profile {profile}; supported profiles are 5, 7, and 8"
        )));
    }
    Ok(None)
}

fn should_tonemap(settings: &voom_domain::plan::TranscodeSettings) -> bool {
    settings.hdr_mode.as_deref() == Some("tonemap")
        || settings.preserve_hdr == Some(false)
        || settings.tonemap.is_some()
}

fn dynamic_hdr_error(message: impl Into<String>) -> VoomError {
    VoomError::ToolExecution {
        tool: "dynamic-hdr".into(),
        message: message.into(),
    }
}

/// Bundles mutation-recording context for passing through the dynamic-HDR pipeline.
struct MutationCtx<'a> {
    storage: Option<&'a dyn ScanSessionMutationStorage>,
    scan_session: Option<ScanSessionId>,
    plan_source: &'a Path,
}

fn ensure_tool_available(
    tool: &str,
    runner: &dyn ProcessRunner,
    env_vars: &[(&str, &str)],
) -> Result<()> {
    let args = vec!["--version".to_string()];
    let output = runner.run(tool, &args, TOOL_PROBE_TIMEOUT, env_vars)?;
    if output.status.success() {
        return Ok(());
    }
    Err(VoomError::ToolExecution {
        tool: tool.into(),
        message: format!("{tool} --version exited with {}", output.status),
    })
}

fn apply_dynamic_hdr_reinjection(
    job: &DynamicHdrJob,
    source_path: &Path,
    output_path: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
    storage: Option<&dyn ScanSessionMutationStorage>,
    scan_session: Option<ScanSessionId>,
) -> Result<()> {
    let work_dir = std::env::temp_dir().join(format!("voom-dynamic-hdr-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&work_dir).map_err(|error| dynamic_hdr_error(error.to_string()))?;
    let mutation = MutationCtx {
        storage,
        scan_session,
        plan_source: source_path,
    };
    let result = apply_dynamic_hdr_reinjection_in_dir(
        job,
        source_path,
        output_path,
        env_vars,
        runner,
        &work_dir,
        &mutation,
    );
    let cleanup = std::fs::remove_dir_all(&work_dir);
    if result.is_ok() {
        cleanup.map_err(|error| dynamic_hdr_error(error.to_string()))?;
    } else {
        let _ = cleanup;
    }
    result
}

fn apply_dynamic_hdr_reinjection_in_dir(
    job: &DynamicHdrJob,
    source_path: &Path,
    output_path: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
    work_dir: &Path,
    mutation: &MutationCtx<'_>,
) -> Result<()> {
    let source_hevc = work_dir.join("source.hevc");
    let encoded_hevc = work_dir.join("encoded.hevc");
    let injected_hevc = work_dir.join("injected.hevc");
    let remuxed = work_dir.join(format!("remuxed.{}", job.output_extension));
    extract_source_hevc(
        source_path,
        job.source_track_index,
        &source_hevc,
        env_vars,
        runner,
    )?;
    extract_dynamic_metadata(job.kind, &source_hevc, work_dir, env_vars, runner)?;
    extract_output_hevc(output_path, &encoded_hevc, env_vars, runner)?;
    inject_dynamic_metadata(
        job.kind,
        &encoded_hevc,
        &injected_hevc,
        work_dir,
        env_vars,
        runner,
    )?;
    remux_injected_hevc(output_path, &injected_hevc, &remuxed, env_vars, runner)?;
    replace_output(mutation, output_path, &remuxed)
}

fn extract_source_hevc(
    source_path: &Path,
    track_index: u32,
    dest: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        source_path.to_string_lossy().to_string(),
        "-map".to_string(),
        format!("0:{track_index}"),
        "-c:v".to_string(),
        "copy".to_string(),
        "-bsf:v".to_string(),
        "hevc_mp4toannexb".to_string(),
        "-f".to_string(),
        "hevc".to_string(),
        dest.to_string_lossy().to_string(),
    ];
    run_checked("ffmpeg", args, env_vars, runner)
}

fn extract_output_hevc(
    output_path: &Path,
    dest: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        output_path.to_string_lossy().to_string(),
        "-map".to_string(),
        "0:v:0".to_string(),
        "-c:v".to_string(),
        "copy".to_string(),
        "-bsf:v".to_string(),
        "hevc_mp4toannexb".to_string(),
        "-f".to_string(),
        "hevc".to_string(),
        dest.to_string_lossy().to_string(),
    ];
    run_checked("ffmpeg", args, env_vars, runner)
}

fn extract_dynamic_metadata(
    kind: DynamicHdrKind,
    source_hevc: &Path,
    work_dir: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let args = match kind {
        DynamicHdrKind::Hdr10Plus => vec![
            "extract".to_string(),
            source_hevc.to_string_lossy().to_string(),
            "-o".to_string(),
            metadata_path(kind, work_dir).to_string_lossy().to_string(),
        ],
        DynamicHdrKind::DolbyVision => vec![
            "extract-rpu".to_string(),
            source_hevc.to_string_lossy().to_string(),
            "-o".to_string(),
            metadata_path(kind, work_dir).to_string_lossy().to_string(),
        ],
    };
    run_checked(kind.tool_name(), args, env_vars, runner)
}

fn inject_dynamic_metadata(
    kind: DynamicHdrKind,
    encoded_hevc: &Path,
    injected_hevc: &Path,
    work_dir: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let args = match kind {
        DynamicHdrKind::Hdr10Plus => vec![
            "inject".to_string(),
            "-i".to_string(),
            encoded_hevc.to_string_lossy().to_string(),
            "-j".to_string(),
            metadata_path(kind, work_dir).to_string_lossy().to_string(),
            "-o".to_string(),
            injected_hevc.to_string_lossy().to_string(),
        ],
        DynamicHdrKind::DolbyVision => vec![
            "inject-rpu".to_string(),
            "-i".to_string(),
            encoded_hevc.to_string_lossy().to_string(),
            "--rpu-in".to_string(),
            metadata_path(kind, work_dir).to_string_lossy().to_string(),
            "-o".to_string(),
            injected_hevc.to_string_lossy().to_string(),
        ],
    };
    run_checked(kind.tool_name(), args, env_vars, runner)
}

fn metadata_path(kind: DynamicHdrKind, work_dir: &Path) -> PathBuf {
    match kind {
        DynamicHdrKind::Hdr10Plus => work_dir.join("hdr10plus.json"),
        DynamicHdrKind::DolbyVision => work_dir.join("rpu.bin"),
    }
}

fn remux_injected_hevc(
    output_path: &Path,
    injected_hevc: &Path,
    remuxed: &Path,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-i".to_string(),
        output_path.to_string_lossy().to_string(),
        "-i".to_string(),
        injected_hevc.to_string_lossy().to_string(),
        "-map".to_string(),
        "1:v:0".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
        "-map".to_string(),
        "0:s?".to_string(),
        "-map".to_string(),
        "0:d?".to_string(),
        "-map".to_string(),
        "0:t?".to_string(),
        "-map_metadata".to_string(),
        "0".to_string(),
        "-c".to_string(),
        "copy".to_string(),
        remuxed.to_string_lossy().to_string(),
    ];
    run_checked("ffmpeg", args, env_vars, runner)
}

fn replace_output(mutation: &MutationCtx<'_>, output_path: &Path, remuxed: &Path) -> Result<()> {
    record_mutation_for_pending_write(
        mutation.storage,
        mutation.scan_session,
        mutation.plan_source,
        output_path,
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_file(remuxed);
    })?;
    std::fs::remove_file(output_path).map_err(|error| dynamic_hdr_error(error.to_string()))?;
    std::fs::rename(remuxed, output_path).map_err(|error| dynamic_hdr_error(error.to_string()))
}

fn run_checked(
    tool: &str,
    args: Vec<String>,
    env_vars: &[(&str, &str)],
    runner: &dyn ProcessRunner,
) -> Result<()> {
    let output = runner.run(tool, &args, FFMPEG_TIMEOUT, env_vars)?;
    if output.status.success() {
        return Ok(());
    }
    let tail = voom_process::stderr_tail(&output.stderr, STDERR_TAIL_LINES);
    Err(VoomError::ToolExecution {
        tool: tool.into(),
        message: format!("{tool} exited with {}: {tail}", output.status),
    })
}

fn prepare_loudness_actions(
    plan: &mut Plan,
    hw_accel: &HwAccelConfig,
) -> Result<Vec<LoudnessTrackUpdate>> {
    let env_vars: Vec<(&str, &str)> = hw_accel.device_env().into_iter().collect();
    let mut remove_actions = Vec::new();
    let mut updates = Vec::new();
    for action in &mut plan.actions {
        let ActionParams::Transcode { settings, .. } = &mut action.parameters else {
            continue;
        };
        let Some(loudness) = settings.loudness.clone() else {
            continue;
        };
        if loudness.measured.is_some() {
            continue;
        }
        let Some(track_index) = action.track_index else {
            continue;
        };
        let measurement = measure_loudness(&plan.file.path, track_index, &loudness, &env_vars)?;
        if loudness.is_within_target(measurement.input_i) {
            if action.description.starts_with("Normalize audio track ") {
                remove_actions.push(action.description.clone());
            } else {
                settings.loudness = None;
            }
            continue;
        }
        updates.push(LoudnessTrackUpdate {
            track_index,
            integrated_lufs: loudness.target_lufs,
            true_peak_db: loudness.true_peak_db,
            loudness_range_lu: loudness.lra_max,
        });
        settings.loudness = Some(loudness.with_measurement(measurement));
    }
    if !remove_actions.is_empty() {
        plan.actions
            .retain(|action| !remove_actions.contains(&action.description));
    }
    Ok(updates)
}

fn measure_loudness(
    path: &std::path::Path,
    track_index: u32,
    settings: &voom_domain::plan::LoudnessNormalization,
    env_vars: &[(&str, &str)],
) -> Result<LoudnessMeasurement> {
    let lra = settings.lra_max.unwrap_or(99.0);
    let filter = format!(
        "loudnorm=I={:.1}:TP={:.1}:LRA={:.1}:print_format=json",
        settings.target_lufs, settings.true_peak_db, lra
    );
    let args = vec![
        "-hide_banner".to_string(),
        "-i".to_string(),
        path.to_string_lossy().to_string(),
        "-map".to_string(),
        format!("0:{track_index}"),
        "-af".to_string(),
        filter,
        "-f".to_string(),
        "null".to_string(),
        "-".to_string(),
    ];
    let output = run_with_timeout_env("ffmpeg", &args, FFMPEG_TIMEOUT, env_vars)?;
    if !output.status.success() {
        let tail = voom_process::stderr_tail(&output.stderr, STDERR_TAIL_LINES);
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("loudness measurement failed: {tail}"),
        });
    }
    parse_loudnorm_json(&String::from_utf8_lossy(&output.stderr))
}

fn parse_loudnorm_json(stderr: &str) -> Result<LoudnessMeasurement> {
    let Some(start) = stderr.rfind('{') else {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: "loudnorm output did not contain JSON".into(),
        });
    };
    let Some(end) = stderr[start..].find('}') else {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: "loudnorm JSON was incomplete".into(),
        });
    };
    let json = &stderr[start..=start + end];
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("failed to parse loudnorm JSON: {error}"),
        })?;
    let read = |key: &str| -> Result<f64> {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!("loudnorm JSON missing numeric {key}"),
            })
    };
    Ok(LoudnessMeasurement::new(
        read("input_i")?,
        read("input_tp")?,
        read("input_lra")?,
        read("input_thresh")?,
        read("target_offset")?,
    ))
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
        let stats = prepare_vmaf_action(&plan.file, action, runner);
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
    file: &voom_domain::media::MediaFile,
    action: &mut PlannedAction,
    runner: &dyn VmafRunner,
) -> OutcomeStats {
    let ActionParams::Transcode { settings, .. } = &mut action.parameters else {
        return OutcomeStats::default();
    };
    let Some(target_vmaf) = settings.resolve_target_vmaf(file) else {
        return OutcomeStats::default();
    };
    settings.target_vmaf = Some(target_vmaf);
    let bounds = BitrateBounds {
        min_bitrate: settings.min_bitrate.clone(),
        max_bitrate: settings.max_bitrate.clone(),
    };
    match runner.run(&file.path, target_vmaf, bounds) {
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
    storage: Option<&dyn ScanSessionMutationStorage>,
) -> Result<std::path::PathBuf> {
    let original_ext = plan
        .file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if ext == original_ext {
        // Same extension: rename temp over original
        record_mutation_for_pending_write(
            storage,
            plan.scan_session,
            &plan.file.path,
            &plan.file.path,
        )
        .inspect_err(|_| {
            let _ = std::fs::remove_file(output_path);
        })?;
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
        record_mutation_for_pending_write(storage, plan.scan_session, &plan.file.path, &new_path)
            .inspect_err(|_| {
            let _ = std::fs::remove_file(output_path);
        })?;
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
    use std::cell::RefCell;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{ExitStatus, Output};

    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::{
        ActionParams, OperationType, Plan, PlannedAction, TranscodeFallback, TranscodeSettings,
    };

    use super::*;
    use crate::vmaf_iterate::{BitrateBounds, IterationError, IterationResult};

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    struct FailingVmafRunner;
    struct SuccessfulVmafRunner;
    struct AnimationOverrideVmafRunner;

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

    impl VmafRunner for AnimationOverrideVmafRunner {
        fn run(
            &self,
            _source: &Path,
            target_vmaf: u32,
            _bounds: BitrateBounds,
        ) -> std::result::Result<IterationResult, IterationError> {
            assert_eq!(target_vmaf, 88);
            Ok(IterationResult {
                final_crf: 20,
                final_bitrate: None,
                achieved_vmaf: 88.1,
                iterations: 2,
            })
        }
    }

    #[derive(Default)]
    struct RecordingProcessRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl RecordingProcessRunner {
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }

        fn success() -> Output {
            Output {
                status: success_status(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        }
    }

    impl ProcessRunner for RecordingProcessRunner {
        fn run(
            &self,
            tool: &str,
            args: &[String],
            _timeout: Duration,
            _env_vars: &[(&str, &str)],
        ) -> Result<Output> {
            self.calls
                .borrow_mut()
                .push((tool.to_string(), args.to_vec()));
            if let Some(path) = command_output_path(tool, args) {
                fs::write(path, format!("{tool} output")).unwrap();
            }
            Ok(Self::success())
        }
    }

    struct FailingProcessRunner {
        stderr: Vec<u8>,
    }

    impl ProcessRunner for FailingProcessRunner {
        fn run(
            &self,
            _tool: &str,
            _args: &[String],
            _timeout: Duration,
            _env_vars: &[(&str, &str)],
        ) -> Result<Output> {
            Ok(Output {
                status: failure_status(),
                stdout: Vec::new(),
                stderr: self.stderr.clone(),
            })
        }
    }

    #[cfg(unix)]
    fn success_status() -> ExitStatus {
        ExitStatus::from_raw(0)
    }

    #[cfg(unix)]
    fn failure_status() -> ExitStatus {
        ExitStatus::from_raw(1 << 8)
    }

    fn command_output_path(tool: &str, args: &[String]) -> Option<PathBuf> {
        if tool == "ffmpeg" {
            return args.last().map(PathBuf::from);
        }
        args.windows(2)
            .find(|pair| pair[0] == "-o")
            .map(|pair| PathBuf::from(&pair[1]))
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

    fn dynamic_hdr_plan(track: Track, settings: TranscodeSettings, codec: &str) -> Plan {
        let mut file = MediaFile::new(PathBuf::from("/media/video.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![track];
        Plan::new(file, "policy", "phase").with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: codec.to_string(),
                settings,
            },
            "transcode video",
        ))
    }

    fn hdr10plus_track() -> Track {
        let mut track = Track::new(0, TrackType::Video, "hevc".into());
        track.is_hdr = true;
        track.hdr_format = Some("HDR10+".into());
        track
    }

    fn dolby_vision_track(profile: u8) -> Track {
        let mut track = Track::new(0, TrackType::Video, "hevc".into());
        track.is_hdr = true;
        track.hdr_format = Some(format!("Dolby Vision Profile {profile}"));
        track.dolby_vision_profile = Some(profile);
        track
    }

    #[test]
    fn dynamic_hdr_job_detects_hdr10plus_hevc_preservation() {
        let plan = dynamic_hdr_plan(hdr10plus_track(), TranscodeSettings::default(), "hevc");
        let actions: Vec<&PlannedAction> = plan.actions.iter().collect();

        let job = dynamic_hdr_job(&plan, &actions, "mkv").unwrap().unwrap();

        assert_eq!(job.kind, DynamicHdrKind::Hdr10Plus);
        assert_eq!(job.source_track_index, 0);
    }

    #[test]
    fn dynamic_hdr_job_rejects_unsupported_dolby_vision_profile() {
        let plan = dynamic_hdr_plan(dolby_vision_track(4), TranscodeSettings::default(), "hevc");
        let actions: Vec<&PlannedAction> = plan.actions.iter().collect();

        let error = dynamic_hdr_job(&plan, &actions, "mkv").unwrap_err();

        let message = error.to_string();
        assert!(message.contains("unsupported Dolby Vision profile 4"));
        assert!(message.contains("supported profiles are 5, 7, and 8"));
    }

    #[test]
    fn dynamic_hdr_job_skips_tonemapped_hdr10plus() {
        let settings = TranscodeSettings::default().with_hdr_mode(Some("tonemap".into()));
        let plan = dynamic_hdr_plan(hdr10plus_track(), settings, "hevc");
        let actions: Vec<&PlannedAction> = plan.actions.iter().collect();

        let job = dynamic_hdr_job(&plan, &actions, "mkv").unwrap();

        assert_eq!(job, None);
    }

    #[test]
    fn dynamic_hdr_job_skips_profile_validation_when_preservation_disabled() {
        let settings = TranscodeSettings::default().with_preserve_hdr(Some(false));
        let plan = dynamic_hdr_plan(dolby_vision_track(4), settings, "hevc");
        let actions: Vec<&PlannedAction> = plan.actions.iter().collect();

        let job = dynamic_hdr_job(&plan, &actions, "mkv").unwrap();

        assert_eq!(job, None);
    }

    #[test]
    fn dynamic_hdr_reinjection_replaces_output_and_cleans_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.mkv");
        let output_path = dir.path().join("output.mkv");
        fs::write(&source_path, "source").unwrap();
        fs::write(&output_path, "muxed output").unwrap();
        let runner = RecordingProcessRunner::default();
        let job = DynamicHdrJob {
            kind: DynamicHdrKind::DolbyVision,
            source_track_index: 0,
            output_extension: "mkv".into(),
        };

        apply_dynamic_hdr_reinjection(&job, &source_path, &output_path, &[], &runner, None, None)
            .unwrap();

        let calls = runner.calls();
        assert_eq!(calls[0].0, "ffmpeg");
        assert_eq!(calls[1].0, "dovi_tool");
        assert!(calls[1].1.contains(&"extract-rpu".to_string()));
        assert_eq!(calls[3].0, "dovi_tool");
        assert!(calls[3].1.contains(&"inject-rpu".to_string()));
        assert_eq!(fs::read_to_string(&output_path).unwrap(), "ffmpeg output");

        let sidecar_path = calls[0].1.last().map(PathBuf::from).unwrap();
        assert!(!sidecar_path.parent().unwrap().exists());
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

    #[test]
    fn prepare_vmaf_transcodes_resolves_animation_override() {
        let settings = TranscodeSettings::default()
            .with_target_vmaf(Some(93))
            .with_vmaf_overrides(Some(std::collections::HashMap::from([(
                "animation".into(),
                88,
            )])));
        let mut plan = video_plan(settings);
        let Some(track) = plan.file.tracks.first_mut() else {
            panic!("expected video track");
        };
        track.is_animation = Some(true);

        let prepared = prepare_vmaf_transcodes_with_outcomes(&plan, &AnimationOverrideVmafRunner);

        let outcome = &prepared.outcomes[0];
        assert_eq!(outcome.target_vmaf, Some(88));
    }

    #[test]
    fn ffmpeg_failures_preserve_full_stderr_alongside_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_path = tmp.path().join("source.mp4");
        fs::write(&source_path, b"input video").unwrap();

        let mut plan = video_plan(TranscodeSettings::default());
        plan.file.path = source_path;
        let full_stderr = (0..=24)
            .map(|i| format!("ffmpeg diagnostic line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let runner = FailingProcessRunner {
            stderr: full_stderr.clone().into_bytes(),
        };

        let execution = execute_plan_with_runners(
            &plan,
            &HwAccelConfig::new(),
            &SuccessfulVmafRunner,
            &runner,
            None,
        )
        .unwrap();

        assert_eq!(execution.action_results.len(), 1);
        assert!(!execution.action_results[0].success);
        let detail = execution.action_results[0]
            .execution_detail
            .as_ref()
            .expect("failed ffmpeg result should include execution detail");
        assert_eq!(detail.stderr_full.as_deref(), Some(full_stderr.as_str()));
        assert!(!detail.stderr_tail.contains("diagnostic line 0"));
        assert!(detail.stderr_tail.contains("diagnostic line 24"));
    }

    #[test]
    fn rename_output_refuses_to_rename_when_storage_errors() {
        use std::path::Path;

        use voom_domain::errors::{StorageErrorKind, VoomError};
        use voom_domain::scan_session_mutations::VoomOriginatedMutation;
        use voom_domain::storage::ScanSessionMutationStorage;
        use voom_domain::transition::ScanSessionId;

        struct FailingStore;
        impl ScanSessionMutationStorage for FailingStore {
            fn record_voom_mutation(&self, _: &VoomOriginatedMutation) -> Result<()> {
                Err(VoomError::Storage {
                    kind: StorageErrorKind::Other,
                    message: "injected failure".into(),
                })
            }
            fn is_voom_originated(&self, _: ScanSessionId, _: &Path) -> Result<bool> {
                Ok(false)
            }
            fn voom_mutations_for_session(
                &self,
                _: ScanSessionId,
            ) -> Result<Vec<VoomOriginatedMutation>> {
                Ok(Vec::new())
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let source_path = tmp.path().join("source.mkv");
        let output_path = tmp.path().join("source.voom_tmp_xyz.mkv");

        // Pre-populate both files with distinct bytes so we can detect any
        // accidental rename.
        fs::write(&source_path, b"ORIGINAL").unwrap();
        fs::write(&output_path, b"TRANSCODED").unwrap();

        let file = MediaFile::new(source_path.clone());
        let plan =
            Plan::new(file, "test-policy", "test-phase").with_scan_session(ScanSessionId::new());

        let storage: &dyn ScanSessionMutationStorage = &FailingStore;
        let result = rename_output(&plan, &output_path, "mkv", Some(storage));

        assert!(
            result.is_err(),
            "rename_output must fail-closed when storage errors"
        );
        assert_eq!(
            fs::read(&source_path).unwrap(),
            b"ORIGINAL",
            "source must be byte-identical — rename must not have happened"
        );
        // Output path may or may not be cleaned up by the executor's existing
        // error-handling cleanup; either is fine as long as the source is intact.
    }
}
