//! Pre- and post-execution safeguards for the process pipeline.
//!
//! Each safeguard short-circuits the phase and records a failure through
//! the same three-step epilogue:
//!
//! 1. Dispatch `PlanFailed` via [`PlanDispatcher::failed`].
//! 2. Record `PhaseOutcomeKind::Failed` for phase stats.
//! 3. Record a failure transition in the store.
//!
//! The [`record_safeguard_failure`] helper folds those three steps into a
//! single call so individual safeguards stay focused on their detection
//! logic.

use voom_domain::events::{Event, PlanFailedEvent};
use voom_domain::utils::format::format_size;

use super::context::{
    record_failure_transition, record_phase_stat, FailureTransitionContext, PhaseOutcomeKind,
    PhaseStatsMap, ProcessContext, TransitionRecorder,
};
use super::dispatch::{dispatch_and_log, PlanDispatcher};
use super::post_execution_path::resolve_post_execution_path;

/// Dependencies needed by execution safeguards.
pub(super) struct SafeguardContext<'a> {
    kernel: &'a voom_kernel::Kernel,
    phase_stats: &'a PhaseStatsMap,
    transitions: TransitionRecorder<'a>,
    token: &'a tokio_util::sync::CancellationToken,
    ffprobe_path: Option<&'a str>,
    flag_size_increase: bool,
    flag_duration_shrink: bool,
}

impl<'a> SafeguardContext<'a> {
    pub(super) fn from_process(ctx: &'a ProcessContext<'a>) -> Self {
        Self {
            kernel: ctx.kernel.as_ref(),
            phase_stats: &ctx.counters.phase_stats,
            transitions: TransitionRecorder {
                store: ctx.store.as_ref(),
                session_id: ctx.counters.session_id,
            },
            token: ctx.token,
            ffprobe_path: ctx.ffprobe_path,
            flag_size_increase: ctx.flag_size_increase,
            flag_duration_shrink: ctx.flag_duration_shrink,
        }
    }
}

/// Dispatch `PlanFailed`, record phase stats, and write a failure transition.
///
/// This is the shared epilogue used by every safeguard when it decides to
/// abort a phase.
fn record_safeguard_failure(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    message: &str,
    ctx: &SafeguardContext<'_>,
) {
    PlanDispatcher::new(ctx.kernel).failed(PlanFailedEvent::new(
        plan.id,
        file.path.clone(),
        plan.phase_name.clone(),
        message,
    ));
    record_phase_stat(ctx.phase_stats, &plan.phase_name, PhaseOutcomeKind::Failed);
    record_failure_transition(&FailureTransitionContext {
        file,
        plan,
        executor: "",
        error_message: Some(message),
        recorder: &ctx.transitions,
    });
}

/// Check if the output file grew larger than the original.
///
/// Returns `true` if the size increased and the phase should be skipped
/// (`PlanFailed` is dispatched and the failure is recorded; `PlanCreated`
/// was already dispatched by the caller). Returns `false` to proceed normally.
pub(super) fn check_size_increase(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &SafeguardContext<'_>,
) -> bool {
    if !ctx.flag_size_increase {
        return false;
    }
    let check_path = resolve_post_execution_path(file, std::slice::from_ref(plan));
    let Ok(meta) = std::fs::metadata(&check_path) else {
        return false;
    };
    let new_size = meta.len();
    if new_size <= file.size || file.size == 0 {
        return false;
    }
    tracing::warn!(
        path = %check_path.display(),
        before = file.size,
        after = new_size,
        "output larger than original, restoring"
    );
    if check_path != file.path {
        if let Err(e) = std::fs::remove_file(&check_path) {
            tracing::warn!(
                path = %check_path.display(),
                error = %e,
                "failed to remove converted output"
            );
        }
    }
    let err_msg = format!("output grew from {} to {} bytes", file.size, new_size);
    record_safeguard_failure(plan, file, &err_msg, ctx);
    true
}

/// Threshold (percent) below which a duration drop triggers the safeguard.
const DURATION_SHRINK_THRESHOLD_PCT: f64 = 5.0;

/// Compute how much shorter `new` is than `orig`, as a percentage of `orig`.
///
/// Returns `0.0` if `orig <= 0.0` or if `new >= orig` (no shrinkage).
#[must_use]
fn duration_shrunk_pct(orig: f64, new: f64) -> f64 {
    if orig <= 0.0 || new >= orig {
        return 0.0;
    }
    (orig - new) / orig * 100.0
}

/// Outcome of the blocking duration-shrink probe.
///
/// Returned by [`check_duration_shrink_blocking`] so the async caller can
/// dispatch events and record stats off the blocking pool.
#[derive(Debug)]
enum DurationShrinkOutcome {
    /// Output duration is significantly shorter than the input.
    Shrunk {
        check_path: std::path::PathBuf,
        new_duration: f64,
        pct: f64,
    },
    /// ffprobe could not introspect the output file — treated as a violation
    /// because the safeguard's purpose is to catch corrupt outputs.
    FfprobeFailed {
        check_path: std::path::PathBuf,
        error: String,
    },
}

/// Pure blocking probe: re-introspect the output file with ffprobe and decide
/// whether the duration safeguard should fire.
///
/// Takes only owned/borrowed primitives so it is safe to run inside
/// `tokio::task::spawn_blocking`. Returns `None` to proceed normally.
fn check_duration_shrink_blocking(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ffprobe_path: Option<&str>,
) -> Option<DurationShrinkOutcome> {
    let check_path = resolve_post_execution_path(file, std::slice::from_ref(plan));
    let meta = std::fs::metadata(&check_path).ok()?;
    let new_size = meta.len();

    // Re-introspect the output file with a one-shot ffprobe call to get its duration.
    let mut introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    if let Some(fp) = ffprobe_path {
        introspector = introspector.with_ffprobe_path(fp);
    }
    let new_duration = match introspector.introspect(&check_path, new_size, None) {
        Ok(event) => event.file.duration,
        Err(e) => {
            tracing::warn!(
                path = %check_path.display(),
                error = %e,
                "duration-shrink check: ffprobe failed on output, treating as violation"
            );
            return Some(DurationShrinkOutcome::FfprobeFailed {
                check_path,
                error: e.to_string(),
            });
        }
    };

    let pct = duration_shrunk_pct(file.duration, new_duration);
    if pct < DURATION_SHRINK_THRESHOLD_PCT {
        return None;
    }

    Some(DurationShrinkOutcome::Shrunk {
        check_path,
        new_duration,
        pct,
    })
}

/// Check if the output file's duration shrank significantly versus the input.
///
/// Returns `true` if the duration dropped by at least `DURATION_SHRINK_THRESHOLD_PCT`
/// (or ffprobe failed to introspect the output, which is itself treated as a
/// violation) and the phase should be marked failed (`PlanFailed` is dispatched
/// and the failure is recorded; `PlanCreated` was already dispatched by the
/// caller). Returns `false` to proceed normally.
///
/// The blocking ffprobe invocation runs on the tokio blocking pool via
/// [`tokio::task::spawn_blocking`] so it does not stall an async worker.
pub(super) async fn check_duration_shrink(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &SafeguardContext<'_>,
) -> bool {
    if !ctx.flag_duration_shrink {
        return false;
    }
    if file.duration <= 0.0 {
        return false;
    }
    if ctx.token.is_cancelled() {
        return false;
    }

    // Move owned data into the blocking task. The probe must not touch &ctx.
    let plan_for_probe = plan.clone();
    let file_for_probe = file.clone();
    let ffprobe_path = ctx.ffprobe_path.map(str::to_owned);
    let outcome = match tokio::task::spawn_blocking(move || {
        check_duration_shrink_blocking(&plan_for_probe, &file_for_probe, ffprobe_path.as_deref())
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "duration-shrink probe join error, skipping check");
            return false;
        }
    };

    let Some(outcome) = outcome else {
        return false;
    };

    let (check_path, err_msg) = match outcome {
        DurationShrinkOutcome::Shrunk {
            check_path,
            new_duration,
            pct,
        } => {
            tracing::warn!(
                path = %check_path.display(),
                before = file.duration,
                after = new_duration,
                pct,
                "output duration shrank, restoring"
            );
            let msg = format!(
                "output duration shrank from {:.2}s to {:.2}s ({:.1}%)",
                file.duration, new_duration, pct
            );
            (check_path, msg)
        }
        DurationShrinkOutcome::FfprobeFailed { check_path, error } => {
            let msg = format!(
                "duration-shrink check: ffprobe failed to introspect output {}: {error}",
                check_path.display()
            );
            (check_path, msg)
        }
    };

    if check_path != file.path {
        if let Err(e) = std::fs::remove_file(&check_path) {
            tracing::warn!(
                path = %check_path.display(),
                error = %e,
                "failed to remove converted output"
            );
        }
    }

    record_safeguard_failure(plan, file, &err_msg, ctx);
    true
}

/// Check whether sufficient disk space is available before executing a plan.
///
/// Returns `true` if space is insufficient and the phase should be skipped
/// (`PlanFailed` is dispatched and the failure is recorded; `PlanCreated`
/// is intentionally not dispatched to avoid triggering executors).
/// Returns `false` to proceed normally.
pub(super) fn check_disk_space(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    ctx: &SafeguardContext<'_>,
) -> bool {
    let check_path = file.path.parent().unwrap_or(std::path::Path::new("/"));

    let available = match voom_domain::utils::disk::available_space(check_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %file.path.display(),
                error = %e,
                "disk space check failed, proceeding anyway"
            );
            return false;
        }
    };

    let required = voom_domain::utils::disk::estimate_required_space(plan, file.size);

    if available >= required {
        return false;
    }

    let message = format!(
        "insufficient disk space: need {} but only {} available on {}",
        format_size(required),
        format_size(available),
        check_path.display(),
    );

    tracing::warn!(
        path = %file.path.display(),
        phase = %plan.phase_name,
        required,
        available,
        "{message}"
    );

    // Note: we intentionally do NOT dispatch PlanCreated here.
    // PlanCreated triggers executor plugins (mkvtoolnix, ffmpeg)
    // which would execute the plan before we can abort it.
    // sqlite-store's update_plan_status is a no-op for unknown
    // plan IDs, so the missing PlanCreated is harmless.
    record_safeguard_failure(plan, file, &message, ctx);
    true
}

/// Dispatch safeguard violations for a plan through the event bus.
pub(super) fn dispatch_safeguard_violations(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    kernel: &voom_kernel::Kernel,
) {
    if plan.safeguard_violations.is_empty() {
        return;
    }
    let mut tagged = file.clone();
    tagged.plugin_metadata.insert(
        "safeguard_violations".to_string(),
        serde_json::json!(&plan.safeguard_violations),
    );
    dispatch_and_log(
        kernel,
        Event::FileIntrospected(voom_domain::events::FileIntrospectedEvent::new(tagged)),
    );
}

/// Annotate plans with `DiskSpaceLow` safeguard violations for dry-run reporting.
///
/// Unlike real execution (which skips the plan entirely), dry-run mode attaches
/// the violation to the plan so it appears in `--plan-only` JSON output.
pub(super) fn annotate_disk_space_violations(
    result: &mut voom_phase_orchestrator::OrchestrationResult,
    file: &voom_domain::media::MediaFile,
) {
    let check_path = file.path.parent().unwrap_or(std::path::Path::new("/"));

    let available = match voom_domain::utils::disk::available_space(check_path) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                path = %file.path.display(),
                error = %e,
                "disk space check failed during dry-run, skipping annotation"
            );
            return;
        }
    };

    for plan in &mut result.plans {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        let required = voom_domain::utils::disk::estimate_required_space(plan, file.size);
        if available < required {
            let message = format!(
                "insufficient disk space: need {} but only {} available on {}",
                format_size(required),
                format_size(available),
                check_path.display(),
            );
            plan.safeguard_violations
                .push(voom_domain::SafeguardViolation::new(
                    voom_domain::SafeguardKind::DiskSpaceLow,
                    message,
                    &plan.phase_name,
                ));
        }
    }
}

/// Collect safeguard violations across plans and tag the file.
pub(super) fn collect_safeguard_violations(
    file: &voom_domain::media::MediaFile,
    result: &voom_phase_orchestrator::OrchestrationResult,
    kernel: &voom_kernel::Kernel,
) {
    let violations: Vec<&voom_domain::SafeguardViolation> = result
        .plans
        .iter()
        .flat_map(|p| &p.safeguard_violations)
        .collect();
    if !violations.is_empty() {
        let mut tagged_file = file.clone();
        tagged_file.plugin_metadata.insert(
            "safeguard_violations".to_string(),
            serde_json::json!(violations),
        );
        dispatch_and_log(
            kernel,
            Event::FileIntrospected(voom_domain::events::FileIntrospectedEvent::new(tagged_file)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)] // exact zero is the documented sentinel
    fn test_duration_shrunk_pct_no_shrink() {
        assert_eq!(duration_shrunk_pct(100.0, 100.0), 0.0);
        assert_eq!(duration_shrunk_pct(100.0, 110.0), 0.0);
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact zero is the documented sentinel
    fn test_duration_shrunk_pct_invalid_orig() {
        assert_eq!(duration_shrunk_pct(0.0, 50.0), 0.0);
        assert_eq!(duration_shrunk_pct(-1.0, 50.0), 0.0);
    }

    #[test]
    fn test_duration_shrunk_pct_below_threshold() {
        // 4% drop — under the 5% threshold
        let pct = duration_shrunk_pct(100.0, 96.0);
        assert!((pct - 4.0).abs() < 1e-9);
        assert!(pct < DURATION_SHRINK_THRESHOLD_PCT);
    }

    #[test]
    fn test_duration_shrunk_pct_above_threshold() {
        // 10% drop — exceeds the 5% threshold
        let pct = duration_shrunk_pct(100.0, 90.0);
        assert!((pct - 10.0).abs() < 1e-9);
        assert!(pct >= DURATION_SHRINK_THRESHOLD_PCT);
    }

    #[test]
    fn test_check_duration_shrink_blocking_no_metadata_returns_none() {
        use voom_domain::media::MediaFile;

        // Point at a path that doesn't exist; the metadata read fails and the
        // probe must return None (no safeguard, no ffprobe invocation).
        let mut file = MediaFile::new(std::path::PathBuf::from("/nonexistent/voom-test.mkv"));
        file.duration = 100.0;
        let plan = super::super::tests::test_plan("normalize", false);
        let outcome = check_duration_shrink_blocking(&plan, &file, None);
        assert!(outcome.is_none());
    }
}
