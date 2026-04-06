//! FFmpeg plan execution: build commands, run subprocess, manage temp files.

use std::time::{Duration, Instant};

use voom_domain::errors::{Result, VoomError};
use voom_domain::plan::{ActionResult, ExecutionDetail, Plan, PlannedAction};
use voom_process::run_with_timeout_env;

use voom_domain::temp_file::temp_path_with_ext;

use crate::command::{build_ffmpeg_command, output_extension};
use crate::hwaccel::HwAccelConfig;

/// Default timeout for `FFmpeg` operations (4 hours — transcode can be slow).
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// Maximum number of stderr lines to capture in `ExecutionDetail`.
const STDERR_TAIL_LINES: usize = 20;

/// Execute a plan by spawning an `FFmpeg` subprocess.
///
/// Builds `FFmpeg` args, runs the command writing to a temp file, then
/// renames the temp file over the original (or to the new extension
/// if converting containers).
pub fn execute_plan(plan: &Plan, hw_accel: &HwAccelConfig) -> Result<Vec<ActionResult>> {
    if !plan.file.path.exists() {
        return Err(VoomError::ToolExecution {
            tool: "ffmpeg".into(),
            message: format!("file not found: {}", plan.file.path.display()),
        });
    }

    let actions: Vec<&PlannedAction> = plan.actions.iter().collect();
    let ext = output_extension(&plan.file, &actions);

    // Build the output path (temp file next to original)
    let output_path = temp_path_with_ext(&plan.file.path, &ext);

    let hw = hw_accel.enabled().then_some(hw_accel);
    let ffmpeg_args = build_ffmpeg_command(&plan.file, &actions, &output_path, hw)?;

    tracing::info!(
        path = %plan.file.path.display(),
        phase = %plan.phase_name,
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
            let final_path = rename_output(plan, &output_path, &ext)?;

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
            Ok(actions
                .iter()
                .map(|a| {
                    ActionResult::success(a.operation, a.description.clone())
                        .with_execution_detail(detail.clone())
                })
                .collect())
        }
        Ok(output) => {
            let _ = std::fs::remove_file(&output_path);
            tracing::debug!(
                args = ?ffmpeg_args,
                "ffmpeg failed"
            );
            let tail = voom_process::stderr_tail(&output.stderr, STDERR_TAIL_LINES);
            let detail = if tail.is_empty() {
                "(no output)".to_string()
            } else {
                tail.clone()
            };
            Err(VoomError::ToolExecution {
                tool: "ffmpeg".into(),
                message: format!(
                    "ffmpeg exited with {}:\n{}\ncmd: {}",
                    output.status, detail, command_str
                ),
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&output_path);
            Err(e)
        }
    }
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

    if ext != original_ext {
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
    } else {
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
    }
}
