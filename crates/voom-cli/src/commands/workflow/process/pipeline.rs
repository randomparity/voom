//! Per-file processing pipeline.
//!
//! Given a discovered file and a compiled policy, the pipeline:
//!
//! 1. Introspects the file via ffprobe.
//! 2. Resolves which policy applies.
//! 3. Either evaluates all phases up-front (dry-run) or executes phases
//!    sequentially, re-introspecting after each successful phase so later
//!    phases see the on-disk state.
//!
//! Plan lifecycle dispatch goes through [`super::dispatch::PlanDispatcher`];
//! safeguards live in [`super::safeguards`]; file transitions are recorded
//! through [`super::transitions`].

use std::collections::HashMap;
use std::sync::atomic::Ordering as AtomicOrdering;

use anyhow::Context;
use voom_domain::events::PlanFailedEvent;
use voom_domain::plan::{ActionParams, OperationType, PhaseOutput, Plan};

use super::audio_language::apply_detected_languages;
use super::context::{
    record_failure_transition, record_phase_stat, FailureTransitionContext, PhaseOutcomeKind,
    ProcessContext, TransitionRecorder,
};
use super::dispatch::{persist_plan, PlanDispatcher};
use super::plan_outcome::PlanOutcome;
use super::post_execution_path::resolve_post_execution_path;
use super::safeguards::{
    annotate_disk_space_violations, check_disk_space, check_duration_shrink, check_size_increase,
    collect_safeguard_violations, dispatch_safeguard_violations, SafeguardContext,
};
use super::transitions::{record_file_transition, FileTransitionContext};
use crate::introspect::DiscoveredFilePayload;

/// Extract and deserialize the job payload from a process job.
fn parse_job_payload(job: &voom_domain::job::Job) -> anyhow::Result<DiscoveredFilePayload> {
    let raw_payload = job
        .payload
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing payload"))?;
    serde_json::from_value(raw_payload.clone()).context("invalid payload")
}

/// Process a single file: introspect, orchestrate, and (unless dry-run) execute plans.
///
/// For dry-run/plan-only mode, all phases are evaluated up front against the
/// original file state (matching existing behavior).
///
/// For real execution, phases are evaluated one at a time. After each phase
/// executes, the file is re-introspected so the next phase sees the current
/// on-disk state (updated path, tracks, container, etc.).
pub(super) async fn process_single_file(
    job: voom_domain::job::Job,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let payload = parse_job_payload(&job).map_err(|e| format!("job payload: {e}"))?;

    let path = std::path::PathBuf::from(&payload.path);

    let stored = crate::introspect::load_stored_file(ctx.store.clone(), path.clone()).await;
    let cache_hit = !ctx.force_rescan
        && stored.as_ref().is_some_and(|s| {
            crate::introspect::matches_discovery(s, payload.size, payload.content_hash.as_deref())
        });

    let mut file = if cache_hit {
        tracing::debug!(path = %path.display(), "introspection cache hit; skipping ffprobe");
        stored.expect("cache_hit implies Some")
    } else {
        let mut fresh = crate::introspect::introspect_file(
            path,
            payload.size,
            payload.content_hash,
            &ctx.kernel,
            ctx.ffprobe_path,
        )
        .await
        .map_err(|e| format!("introspect {}: {e}", payload.path))?;

        // Prior runs may have written plugin_metadata that the current
        // introspection didn't reproduce; merge so the evaluator sees both.
        if let Some(stored) = stored {
            for (k, v) in stored.plugin_metadata {
                fresh.plugin_metadata.entry(k).or_insert(v);
            }
        }
        fresh
    };

    apply_detected_languages(&mut file);

    // Resolve which policy applies to this file.
    let matched = ctx
        .resolver
        .resolve(&file.path)
        .map_err(|e| format!("policy resolution: {e}"))?;
    let compiled = match matched {
        crate::policy_map::PolicyMatch::Policy(compiled, _name) => compiled,
        crate::policy_map::PolicyMatch::Skip => {
            return Ok(Some(serde_json::json!({
                "path": file.path.display().to_string(),
                "skipped": true,
                "reason": "excluded by policy map",
            })));
        }
    };

    if ctx.dry_run {
        process_single_file_dry_run(&file, compiled, ctx)
    } else {
        process_single_file_execute(&file, compiled, ctx).await
    }
}

/// Dry-run / plan-only: evaluate all phases up front against the original file.
///
/// Signature mirrors `process_single_file_execute` so that `process_single_file`
/// can dispatch either without casting — the `Result` wrapper is required even
/// though the dry-run path never produces an error.
#[allow(clippy::unnecessary_wraps)]
fn process_single_file_dry_run(
    file: &voom_domain::media::MediaFile,
    compiled: &voom_dsl::CompiledPolicy,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let mut result = orchestrate_plans(compiled, file, ctx.capabilities);
    annotate_disk_space_violations(&mut result, file);

    collect_safeguard_violations(file, &result, ctx.kernel.as_ref());

    let needs_exec = voom_phase_orchestrator::needs_execution(&result);
    if needs_exec {
        ctx.counters
            .modified_count
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    record_dry_run_phase_stats(&result, ctx);
    collect_plan_only_output(&result, ctx);

    let plan_summaries: Vec<serde_json::Value> = result
        .plans
        .iter()
        .map(|p| {
            let mut summary = serde_json::json!({
                "phase": p.phase_name,
                "actions": p.actions.len(),
                "skipped": p.is_skipped(),
            });
            if !p.safeguard_violations.is_empty() {
                summary["safeguard_violations"] = serde_json::json!(p.safeguard_violations);
            }
            summary
        })
        .collect();

    Ok(Some(serde_json::json!({
        "path": file.path.display().to_string(),
        "needs_execution": needs_exec,
        "plans": plan_summaries,
    })))
}

fn record_dry_run_phase_stats(
    result: &voom_phase_orchestrator::OrchestrationResult,
    ctx: &ProcessContext<'_>,
) {
    for plan in &result.plans {
        if plan.is_skipped() {
            let reason = plan
                .skip_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            record_phase_stat(
                &ctx.counters.phase_stats,
                &plan.phase_name,
                PhaseOutcomeKind::Skipped(reason),
            );
        } else if !plan.is_empty() {
            record_phase_stat(
                &ctx.counters.phase_stats,
                &plan.phase_name,
                PhaseOutcomeKind::Completed,
            );
        }
    }
}

fn collect_plan_only_output(
    result: &voom_phase_orchestrator::OrchestrationResult,
    ctx: &ProcessContext<'_>,
) {
    if !ctx.plan_only {
        return;
    }

    let plans_json: Vec<serde_json::Value> = result
        .plans
        .iter()
        .filter(|p| !p.is_empty() && !p.is_skipped())
        .filter_map(|p| {
            serde_json::to_value(p)
                .inspect_err(|e| {
                    tracing::warn!(
                        phase = %p.phase_name,
                        error = %e,
                        "failed to serialize plan for plan-only output"
                    );
                })
                .ok()
        })
        .collect();

    if !plans_json.is_empty() {
        ctx.counters.plan_collector.lock().extend(plans_json);
    }
}

/// Real execution: evaluate → execute → re-introspect per phase.
///
/// After each phase executes successfully, the file is re-introspected so
/// that subsequent phases see the updated path, container, and tracks.
async fn process_single_file_execute(
    file: &voom_domain::media::MediaFile,
    compiled: &voom_dsl::CompiledPolicy,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<Option<serde_json::Value>, String> {
    let file_path_str = file.path.display().to_string();

    // Verify file hasn't changed since introspection (TOCTOU guard)
    if let Some(skip_json) = check_file_hash(file).await {
        return Ok(Some(skip_json));
    }

    let safeguards = SafeguardContext::from_process(ctx);
    let phase_ctx = PhaseExecutionContext {
        compiled,
        keep_backups: compiled.config.keep_backups,
        safeguards: &safeguards,
        process: ctx,
    };
    let mut state = PhaseExecutionState::new(file.clone());

    for phase_name in &compiled.phase_order {
        if ctx.token.is_cancelled() {
            break;
        }

        if run_phase_iteration(phase_name, &mut state, &phase_ctx).await? == PhaseLoopControl::Stop
        {
            break;
        }
    }

    Ok(Some(serde_json::json!({
        "path": file_path_str,
        "needs_execution": state.any_executed,
        "plans_evaluated": state.plans_evaluated,
    })))
}

struct PhaseExecutionState {
    current_file: voom_domain::media::MediaFile,
    outcomes: HashMap<String, voom_policy_evaluator::EvaluationOutcome>,
    phase_outputs: HashMap<String, PhaseOutput>,
    any_executed: bool,
    modified_counted: bool,
    plans_evaluated: usize,
}

impl PhaseExecutionState {
    fn new(current_file: voom_domain::media::MediaFile) -> Self {
        Self {
            current_file,
            outcomes: HashMap::new(),
            phase_outputs: HashMap::new(),
            any_executed: false,
            modified_counted: false,
            plans_evaluated: 0,
        }
    }
}

struct PhaseExecutionContext<'a> {
    compiled: &'a voom_dsl::CompiledPolicy,
    keep_backups: bool,
    safeguards: &'a SafeguardContext<'a>,
    process: &'a ProcessContext<'a>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PhaseLoopControl {
    Continue,
    Stop,
}

async fn run_phase_iteration(
    phase_name: &str,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
) -> std::result::Result<PhaseLoopControl, String> {
    let Some(plan) = evaluate_phase_plan(phase_name, state, phase_ctx) else {
        return Ok(PhaseLoopControl::Continue);
    };
    let mut plan = plan.with_session_id(phase_ctx.process.counters.session_id);

    state.plans_evaluated += 1;

    dispatch_safeguard_violations(
        &plan,
        &state.current_file,
        phase_ctx.process.kernel.as_ref(),
    );

    if let Some(reason) = &plan.skip_reason {
        record_skipped_phase(&plan, state, phase_ctx, reason)?;
        return Ok(PhaseLoopControl::Continue);
    }

    if plan.is_empty() {
        record_empty_phase(phase_name, state);
        return Ok(PhaseLoopControl::Continue);
    }

    apply_auto_crop_detection(&mut plan, state, phase_ctx).await?;

    if check_disk_space(&plan, &state.current_file, phase_ctx.safeguards) {
        record_safeguard_failed_phase(phase_name, state);
        return Ok(PhaseLoopControl::Continue);
    }

    execute_phase_plan(plan, state, phase_ctx).await
}

async fn apply_auto_crop_detection(
    plan: &mut Plan,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
) -> std::result::Result<(), String> {
    if state.current_file.crop_detection.is_some() {
        return Ok(());
    }
    let Some(request) = cropdetect_request_for_plan(plan, &state.current_file) else {
        return Ok(());
    };

    let path = state.current_file.path.clone();
    let detected = tokio::task::spawn_blocking(move || {
        voom_ffmpeg_executor::cropdetect::detect_crop(
            "ffmpeg",
            &path,
            request.source,
            &request.settings,
        )
    })
    .await
    .map_err(|e| format!("cropdetect join error: {e}"))?
    .map_err(|e| format!("cropdetect {}: {e}", state.current_file.path.display()))?;

    if let Some(crop_detection) = detected {
        state.current_file.crop_detection = Some(crop_detection);
        plan.file = state.current_file.clone();
        phase_ctx
            .process
            .store
            .upsert_file(&state.current_file)
            .map_err(|e| format!("persist crop detection: {e}"))?;
    }
    Ok(())
}

struct CropDetectRequest {
    settings: voom_domain::CropSettings,
    source: voom_ffmpeg_executor::cropdetect::CropDetectSource,
}

fn cropdetect_request_for_plan(
    plan: &Plan,
    file: &voom_domain::media::MediaFile,
) -> Option<CropDetectRequest> {
    plan.actions.iter().find_map(|action| {
        if action.operation != OperationType::TranscodeVideo {
            return None;
        }
        let ActionParams::Transcode { settings, .. } = &action.parameters else {
            return None;
        };
        let crop = settings.crop.as_ref()?;
        let track_index = action.track_index?;
        let track = file
            .tracks
            .iter()
            .find(|track| track.index == track_index)?;
        Some(CropDetectRequest {
            settings: crop.clone(),
            source: voom_ffmpeg_executor::cropdetect::CropDetectSource::new(
                track.width?,
                track.height?,
                file.duration,
            ),
        })
    })
}

fn evaluate_phase_plan(
    phase_name: &str,
    state: &PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
) -> Option<voom_domain::plan::Plan> {
    let phase_output_lookup =
        |name: &str| -> Option<PhaseOutput> { state.phase_outputs.get(name).cloned() };
    voom_policy_evaluator::evaluator::evaluate_single_phase_with_evaluation_context(
        phase_name,
        phase_ctx.compiled,
        &state.current_file,
        voom_policy_evaluator::SinglePhaseEvaluationContext {
            phase_outcomes: &state.outcomes,
            capabilities: Some(phase_ctx.process.capabilities),
            phase_output_lookup: Some(&phase_output_lookup),
        },
    )
}

fn record_skipped_phase(
    plan: &voom_domain::plan::Plan,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
    reason: &str,
) -> std::result::Result<(), String> {
    state.outcomes.insert(
        plan.phase_name.clone(),
        voom_policy_evaluator::EvaluationOutcome::Skipped,
    );
    state.phase_outputs.insert(
        plan.phase_name.clone(),
        phase_output(false, false, Some("skipped")),
    );
    dispatch_skipped_plan(plan, &state.current_file, reason, phase_ctx.process)?;
    Ok(())
}

fn record_empty_phase(phase_name: &str, state: &mut PhaseExecutionState) {
    state.outcomes.insert(
        phase_name.to_string(),
        voom_policy_evaluator::EvaluationOutcome::Executed { modified: false },
    );
    state.phase_outputs.insert(
        phase_name.to_string(),
        phase_output(true, false, Some("unchanged")),
    );
}

fn record_safeguard_failed_phase(phase_name: &str, state: &mut PhaseExecutionState) {
    state.outcomes.insert(
        phase_name.to_string(),
        voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
    );
    state.phase_outputs.insert(
        phase_name.to_string(),
        phase_output(false, false, Some("safeguard_failed")),
    );
}

async fn execute_phase_plan(
    plan: voom_domain::plan::Plan,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
) -> std::result::Result<PhaseLoopControl, String> {
    let permit = tokio::select! {
        biased;
        () = phase_ctx.process.token.cancelled() => return Ok(PhaseLoopControl::Stop),
        permit = phase_ctx.process.plan_limiter.acquire_for_plan(&plan) => permit,
    };
    let plan_clone = plan.clone();
    let file_clone = state.current_file.clone();
    let kernel_clone = phase_ctx.process.kernel.clone();
    let start = std::time::Instant::now();
    let exec_outcome = tokio::task::spawn_blocking(move || {
        execute_single_plan(&plan_clone, &file_clone, &kernel_clone)
    })
    .await
    .map_err(|e| format!("plan execution join error: {e}"))?;
    drop(permit);
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

    match exec_outcome {
        PlanOutcome::Success {
            executor,
            phase_output,
        } => {
            handle_phase_success(plan, state, phase_ctx, &executor, elapsed_ms, phase_output)
                .await?;
        }
        PlanOutcome::Failed(failed) => {
            handle_phase_failure(failed, &plan, state, phase_ctx);
        }
    }

    Ok(PhaseLoopControl::Continue)
}

async fn handle_phase_success(
    plan: voom_domain::plan::Plan,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
    executor: &str,
    elapsed_ms: u64,
    successful_phase_output: PhaseOutput,
) -> std::result::Result<(), String> {
    if check_size_increase(&plan, &state.current_file, phase_ctx.safeguards) {
        record_success_safeguard_failure(&plan.phase_name, state);
        return Ok(());
    }
    if check_duration_shrink(&plan, &state.current_file, phase_ctx.safeguards).await {
        record_success_safeguard_failure(&plan.phase_name, state);
        return Ok(());
    }

    state.any_executed = true;
    if !state.modified_counted {
        phase_ctx
            .process
            .counters
            .modified_count
            .fetch_add(1, AtomicOrdering::Relaxed);
        state.modified_counted = true;
    }
    state.outcomes.insert(
        plan.phase_name.clone(),
        voom_policy_evaluator::EvaluationOutcome::Executed { modified: true },
    );
    state
        .phase_outputs
        .insert(plan.phase_name.clone(), successful_phase_output);
    state.current_file = finalize_successful_plan_execution(
        plan,
        &state.current_file,
        executor,
        elapsed_ms,
        phase_ctx.keep_backups,
        phase_ctx.process,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn record_success_safeguard_failure(phase_name: &str, state: &mut PhaseExecutionState) {
    state.outcomes.insert(
        phase_name.to_string(),
        voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
    );
    state.phase_outputs.insert(
        phase_name.to_string(),
        phase_output(false, false, Some("safeguard_failed")),
    );
}

fn handle_phase_failure(
    failed: PlanFailedEvent,
    plan: &voom_domain::plan::Plan,
    state: &mut PhaseExecutionState,
    phase_ctx: &PhaseExecutionContext<'_>,
) {
    let executor = failed.plugin_name.clone().unwrap_or_default();
    let error_msg = failed.error.clone();
    dispatch_plan_failure(failed, &plan.phase_name, phase_ctx.process);
    record_failure_transition(&FailureTransitionContext {
        file: &state.current_file,
        plan,
        executor: &executor,
        error_message: Some(&error_msg),
        recorder: &TransitionRecorder {
            store: phase_ctx.process.store.as_ref(),
            session_id: phase_ctx.process.counters.session_id,
        },
    });
    state.outcomes.insert(
        plan.phase_name.clone(),
        voom_policy_evaluator::EvaluationOutcome::ExecutionFailed,
    );
    state.phase_outputs.insert(
        plan.phase_name.clone(),
        phase_output(false, false, Some("failed")),
    );
}

fn phase_output(completed: bool, modified: bool, outcome: Option<&str>) -> PhaseOutput {
    let output = PhaseOutput::new()
        .with_completed(completed)
        .with_modified(modified);
    match outcome {
        Some(outcome) => output.with_outcome(outcome),
        None => output,
    }
}

/// Persist a skipped plan, then dispatch `PlanSkipped` without notifying executors.
fn dispatch_skipped_plan(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    reason: &str,
    ctx: &ProcessContext<'_>,
) -> std::result::Result<(), String> {
    persist_plan(ctx.store.as_ref(), plan).map_err(|e| format!("persist skipped plan: {e}"))?;
    let dispatcher = PlanDispatcher::new(&ctx.kernel);
    dispatcher.skipped(plan, file, reason);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Skipped(reason.to_string()),
    );
    Ok(())
}

/// Dispatch a `PlanFailed` event and record the phase stat.
fn dispatch_plan_failure(failed: PlanFailedEvent, phase_name: &str, ctx: &ProcessContext<'_>) {
    PlanDispatcher::new(&ctx.kernel).failed(failed);
    record_phase_stat(
        &ctx.counters.phase_stats,
        phase_name,
        PhaseOutcomeKind::Failed,
    );
}

/// Handle a successfully executed plan: dispatch completion, re-introspect,
/// and record the file transition.
pub(super) async fn finalize_successful_plan_execution(
    plan: voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    executor: &str,
    elapsed_ms: u64,
    keep_backups: bool,
    ctx: &ProcessContext<'_>,
) -> voom_domain::Result<voom_domain::media::MediaFile> {
    if keep_backups {
        ctx.counters
            .backup_bytes
            .fetch_add(file.size, AtomicOrdering::Relaxed);
        tracing::info!(
            path = %file.path.display(),
            phase = %plan.phase_name,
            "keeping backup per policy"
        );
    }
    PlanDispatcher::new(&ctx.kernel).completed(&plan, file, keep_backups);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Completed,
    );

    let plan_id = plan.id;
    let actions_taken = u32::try_from(plan.actions.len()).unwrap_or(u32::MAX);
    let tracks_modified = u32::try_from(
        plan.actions
            .iter()
            .filter(|a| a.track_index.is_some())
            .count(),
    )
    .unwrap_or(u32::MAX);
    let policy_name = plan.policy_name.clone();
    let phase_name = plan.phase_name.clone();

    // The path rename, transition insert, and expected_hash update are
    // bundled into one atomic write below (record_file_transition →
    // record_post_execution). Re-introspection uses the no-dispatch
    // variant so it doesn't auto-upsert the file row at the post-execution
    // path before the bundle's rename can move the existing row.
    let post_exec_path = resolve_post_execution_path(file, std::slice::from_ref(&plan));
    let new_file = reintrospect_file(file, post_exec_path, ctx).await;
    let transition_recorder = TransitionRecorder {
        store: ctx.store.as_ref(),
        session_id: ctx.counters.session_id,
    };

    record_file_transition(&FileTransitionContext {
        old_file: file,
        new_file: &new_file,
        executor,
        elapsed_ms,
        actions_taken,
        tracks_modified,
        policy_name: &policy_name,
        phase_name: &phase_name,
        plan_id,
        recorder: &transition_recorder,
    })?;

    // Defense-in-depth: clear any `bad_files` row at the post-execution
    // path. The bundle in `record_post_execution` already does this for
    // the path-change / hash-change paths atomically, but
    // `record_file_transition` short-circuits when nothing changed
    // (same path AND same hash), so the bundle never runs and an orphan
    // row from a re-introspection failure (or stale scan-time failure)
    // would persist. Idempotent — DELETE WHERE path = … is safe even
    // when no row exists. See issue #180.
    if let Err(e) = ctx.store.delete_bad_file_by_path(&new_file.path) {
        tracing::warn!(
            path = %new_file.path.display(),
            error = %e,
            "failed to clear bad_files row at post-execution path"
        );
    }

    Ok(new_file)
}

/// Check file hash for TOCTOU guard. Returns Some(json) if file should be skipped.
async fn check_file_hash(file: &voom_domain::media::MediaFile) -> Option<serde_json::Value> {
    let file_path_str = file.path.display().to_string();
    let Some(ref stored_hash) = file.content_hash else {
        tracing::debug!(path = %file.path.display(),
            "no content_hash available, skipping TOCTOU check");
        return None;
    };

    let hash_path = file.path.clone();
    let hash_result =
        tokio::task::spawn_blocking(move || voom_discovery::hash_file(&hash_path)).await;
    match hash_result {
        Ok(Ok(current_hash)) if &current_hash != stored_hash => {
            tracing::warn!(path = %file.path.display(), "file changed since introspection, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": "file changed since introspection",
            }))
        }
        Ok(Err(e)) => {
            tracing::warn!(path = %file.path.display(), error = %e, "hash check failed, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": format!("hash check failed: {e}"),
            }))
        }
        Err(e) => {
            tracing::warn!(path = %file.path.display(), error = %e, "hash task panicked, skipping");
            Some(serde_json::json!({
                "path": file_path_str,
                "skipped": true,
                "reason": format!("hash task panicked: {e}"),
            }))
        }
        _ => None, // hash matches, proceed
    }
}

/// Re-introspect the file after a phase executes, returning the updated
/// `MediaFile` with the current on-disk path, tracks, and metadata.
async fn reintrospect_file(
    file: &voom_domain::media::MediaFile,
    current_path: std::path::PathBuf,
    ctx: &ProcessContext<'_>,
) -> voom_domain::media::MediaFile {
    if !current_path.exists() {
        tracing::warn!(
            path = %current_path.display(),
            "file not found after execution, using previous state"
        );
        return file.clone();
    }

    let p = current_path.clone();
    let file_size = file.size;
    let (size, hash) = tokio::task::spawn_blocking(move || {
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(file_size);
        let hash = match voom_discovery::hash_file(&p) {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e,
                    "could not hash file after execution");
                None
            }
        };
        (size, hash)
    })
    .await
    .unwrap_or((file.size, None));

    let ffp = ctx.ffprobe_path.map(String::from);
    let kernel_clone = ctx.kernel.clone();
    match crate::introspect::introspect_file_no_dispatch(
        current_path.clone(),
        size,
        hash.clone(),
        &kernel_clone,
        ffp.as_deref(),
    )
    .await
    {
        Ok(mut new_file) => {
            // Preserve plugin_metadata from prior phases
            for (k, v) in &file.plugin_metadata {
                new_file
                    .plugin_metadata
                    .entry(k.clone())
                    .or_insert(v.clone());
            }
            // Reapply detector-derived languages so subsequent
            // phases see normalized values, not raw ffprobe tags.
            apply_detected_languages(&mut new_file);
            new_file
        }
        Err(e) => {
            // Re-introspection failed but the file is on disk at
            // `current_path`. Preserve the previous tracks/codecs/etc.
            // (best effort — we can't introspect them) but reflect the
            // post-execution path, size, container, and hash so the
            // bundled write downstream still records the rename and the
            // metadata snapshot doesn't misreport the on-disk state.
            tracing::warn!(error = %e, "re-introspection failed, using previous state");
            let mut fallback = file.clone();
            fallback.container = voom_domain::media::Container::from_extension(
                current_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or(""),
            );
            fallback.path = current_path;
            fallback.size = size;
            // Don't clobber a known-good prior hash with None when
            // post-execution hashing failed — the prior hash is still
            // a better signal than nothing.
            if hash.is_some() {
                fallback.content_hash = hash;
            }
            fallback
        }
    }
}

/// Run the phase orchestrator to produce plans (used for dry-run mode).
///
/// NOTE: This function does NOT dispatch `PlanCreated` events. Dispatching
/// here would trigger executor plugins during dry-run mode.
fn orchestrate_plans(
    compiled: &voom_dsl::CompiledPolicy,
    file: &voom_domain::media::MediaFile,
    capabilities: &voom_domain::CapabilityMap,
) -> voom_phase_orchestrator::OrchestrationResult {
    let plans = voom_policy_evaluator::evaluate_with_evaluation_context(
        compiled,
        file,
        voom_policy_evaluator::EvaluationContext {
            capabilities: Some(capabilities),
            phase_output_lookup: None,
        },
    )
    .plans;
    voom_phase_orchestrator::orchestrate(plans)
}

/// Dispatch `PlanExecuting` + `PlanCreated` for a single plan.
///
/// Returns the outcome without dispatching `PlanCompleted` or `PlanFailed`
/// — the caller decides when to commit the result (e.g. after size checks).
///
/// `PlanExecuting` is dispatched first so the backup-manager backs up the file
/// BEFORE any executor modifies it.  `PlanCreated` then lets executor plugins
/// claim and run the plan.
pub(super) fn execute_single_plan(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    kernel: &voom_kernel::Kernel,
) -> PlanOutcome {
    let results = PlanDispatcher::new(kernel).begin(plan, file);
    PlanOutcome::from_event_result(&results, plan, file)
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult, VerifyCompletedDetails, VerifyCompletedEvent};
    use voom_domain::media::{Container, MediaFile, Track, TrackType};
    use voom_domain::plan::{CropSettings, PlannedAction, TranscodeSettings};
    use voom_domain::verification::{VerificationMode, VerificationOutcome};

    use super::super::tests as process_tests;
    use super::*;

    struct RecordingExecutor {
        entered_tx: mpsc::Sender<()>,
    }

    struct VerifyOutcomeExecutor {
        executed_tx: mpsc::Sender<String>,
    }

    impl voom_kernel::Plugin for RecordingExecutor {
        fn name(&self) -> &'static str {
            "recording-executor"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn capabilities(&self) -> &[Capability] {
            &[]
        }

        fn handles(&self, event_type: &str) -> bool {
            event_type == Event::PLAN_CREATED
        }

        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            if let Event::PlanCreated(_) = event {
                let _ = self.entered_tx.send(());
                return Ok(Some(EventResult::plan_succeeded(self.name(), None)));
            }
            Ok(None)
        }
    }

    impl voom_kernel::Plugin for VerifyOutcomeExecutor {
        fn name(&self) -> &'static str {
            "verify-outcome-executor"
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn capabilities(&self) -> &[Capability] {
            &[]
        }

        fn handles(&self, event_type: &str) -> bool {
            event_type == Event::PLAN_CREATED
        }

        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            let Event::PlanCreated(created) = event else {
                return Ok(None);
            };
            if created.plan.skip_reason.is_some() {
                return Ok(None);
            }

            let _ = self.executed_tx.send(created.plan.phase_name.clone());
            if created.plan.phase_name != "verify" {
                return Ok(Some(EventResult::plan_succeeded(self.name(), None)));
            }

            let event = Event::VerifyCompleted(VerifyCompletedEvent::new(
                created.plan.file.id.to_string(),
                created.plan.file.path.clone(),
                VerifyCompletedDetails::new(
                    VerificationMode::Quick,
                    VerificationOutcome::Ok,
                    0,
                    0,
                    uuid::Uuid::new_v4(),
                ),
            ));
            let mut result = EventResult::new(self.name());
            result.claimed = true;
            result.produced_events = vec![event];
            Ok(Some(result))
        }
    }

    fn nvenc_policy() -> &'static str {
        r#"policy "test" {
                phase transcode-video {
                    transcode video to hevc {
                        hw: nvenc
                    }
                }
            }"#
    }

    fn global_hw_policy() -> &'static str {
        r#"policy "test" {
                phase transcode-video {
                    transcode video to hevc
                }
            }"#
    }

    fn h264_file(path: std::path::PathBuf) -> MediaFile {
        MediaFile::new(path)
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())])
    }

    fn crop_plan(file: MediaFile) -> Plan {
        Plan::new(file, "test", "transcode-video").with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crop(Some(CropSettings::auto())),
            },
            "transcode with crop",
        ))
    }

    #[test]
    fn crop_source_for_plan_uses_cropped_transcode_track_dimensions() {
        let mut file = h264_file(std::path::PathBuf::from("/media/source.mkv"));
        file.duration = 120.0;
        file.tracks[0].width = Some(1920);
        file.tracks[0].height = Some(1080);
        let plan = crop_plan(file.clone());

        let request = cropdetect_request_for_plan(&plan, &file).expect("cropdetect request");

        assert_eq!(
            request.source,
            voom_ffmpeg_executor::cropdetect::CropDetectSource::new(1920, 1080, 120.0)
        );
    }

    #[test]
    fn crop_settings_for_plan_ignores_transcodes_without_crop() {
        let file = h264_file(std::path::PathBuf::from("/media/source.mkv"));
        let plan = Plan::new(file, "test", "transcode-video").with_action(PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default(),
            },
            "transcode without crop",
        ));

        assert!(cropdetect_request_for_plan(&plan, &plan.file).is_none());
    }

    async fn held_nvenc_permit(
        limiter: &voom_job_manager::plan_limiter::PlanExecutionLimiter,
    ) -> voom_job_manager::plan_limiter::PlanExecutionPermit {
        limiter
            .acquire_for_plan(&process_tests::test_plan_with_transcode_hw(
                "transcode-video",
                "nvenc",
            ))
            .await
    }

    fn observe_limiter_acquires(
        limiter: voom_job_manager::plan_limiter::PlanExecutionLimiter,
    ) -> (
        voom_job_manager::plan_limiter::PlanExecutionLimiter,
        tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        let (wait_tx, wait_rx) = tokio::sync::mpsc::unbounded_channel();
        let limiter = limiter.with_acquire_observer(move |resource| {
            let _ = wait_tx.send(resource.to_string());
        });
        (limiter, wait_rx)
    }

    async fn wait_for_limiter_acquire(
        wait_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
        resource: &str,
        processing: Pin<
            &mut impl Future<Output = std::result::Result<Option<serde_json::Value>, String>>,
        >,
    ) {
        let acquired = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                acquired = wait_rx.recv() => acquired
                    .expect("plan limiter acquire observer should still be connected"),
                result = processing => {
                    panic!("processing finished before reaching the plan limiter: {result:?}");
                }
            }
        })
        .await
        .expect("processing should reach the plan limiter acquire point");
        assert_eq!(acquired, resource);
    }

    #[tokio::test]
    async fn process_pipeline_respects_plan_limiter() {
        let policy = nvenc_policy();
        let fixture = process_tests::TestFixture::with_policy(policy);
        let limiter = voom_job_manager::plan_limiter::PlanExecutionLimiter::from_limits(vec![(
            "hw:nvenc".to_string(),
            1,
        )]);
        let held = held_nvenc_permit(&limiter).await;
        let (limiter, mut wait_rx) = observe_limiter_acquires(limiter);

        let path = fixture.dir_path().join("movie.mkv");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let file = h264_file(path);
        let compiled = voom_dsl::compile_policy(policy).unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let mut kernel = voom_kernel::Kernel::new();
        kernel
            .register_plugin(Arc::new(RecordingExecutor { entered_tx }), 50)
            .unwrap();
        let ctx = ProcessContext {
            plan_limiter: Arc::new(limiter),
            ..fixture.make_ctx(
                Arc::new(kernel),
                Arc::new(voom_domain::test_support::InMemoryStore::new()),
            )
        };

        let processing = process_single_file_execute(&file, &compiled, &ctx);
        tokio::pin!(processing);

        wait_for_limiter_acquire(&mut wait_rx, "hw:nvenc", processing.as_mut()).await;
        assert!(
            matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "executor must not receive PlanCreated while the nvenc permit is held"
        );

        drop(held);
        tokio::time::timeout(Duration::from_secs(1), &mut processing)
            .await
            .expect("processing should continue after the limiter permit is released")
            .expect("processing should complete without execution errors");
        entered_rx
            .try_recv()
            .expect("executor should receive PlanCreated after permit release");
    }

    #[tokio::test]
    async fn process_pipeline_limits_transcode_without_per_action_hw() {
        let policy = global_hw_policy();
        let fixture = process_tests::TestFixture::with_policy(policy);
        let limiter =
            voom_job_manager::plan_limiter::PlanExecutionLimiter::from_limits_with_default(
                vec![("hw:nvenc".to_string(), 1)],
                Some("hw:nvenc".to_string()),
            );
        let held = limiter
            .acquire_for_plan(&process_tests::test_plan_with_optional_transcode_hw(
                "transcode-video",
                None,
            ))
            .await;
        let (limiter, mut wait_rx) = observe_limiter_acquires(limiter);

        let path = fixture.dir_path().join("movie.mkv");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let file = h264_file(path);
        let compiled = voom_dsl::compile_policy(policy).unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let mut kernel = voom_kernel::Kernel::new();
        kernel
            .register_plugin(Arc::new(RecordingExecutor { entered_tx }), 50)
            .unwrap();
        let ctx = ProcessContext {
            plan_limiter: Arc::new(limiter),
            ..fixture.make_ctx(
                Arc::new(kernel),
                Arc::new(voom_domain::test_support::InMemoryStore::new()),
            )
        };

        let processing = process_single_file_execute(&file, &compiled, &ctx);
        tokio::pin!(processing);

        wait_for_limiter_acquire(&mut wait_rx, "hw:nvenc", processing.as_mut()).await;
        assert!(
            matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "executor must not receive PlanCreated while the default nvenc permit is held"
        );

        drop(held);
        tokio::time::timeout(Duration::from_secs(1), &mut processing)
            .await
            .expect("processing should continue after the limiter permit is released")
            .expect("processing should complete without execution errors");
        entered_rx
            .try_recv()
            .expect("executor should receive PlanCreated after permit release");
    }

    #[tokio::test]
    async fn process_pipeline_cancellation_stops_waiting_for_plan_limiter() {
        let policy = nvenc_policy();
        let fixture = process_tests::TestFixture::with_policy(policy);
        let limiter = voom_job_manager::plan_limiter::PlanExecutionLimiter::from_limits(vec![(
            "hw:nvenc".to_string(),
            1,
        )]);
        let held = held_nvenc_permit(&limiter).await;
        let (limiter, mut wait_rx) = observe_limiter_acquires(limiter);

        let path = fixture.dir_path().join("movie.mkv");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let file = h264_file(path);
        let compiled = voom_dsl::compile_policy(policy).unwrap();

        let (entered_tx, entered_rx) = mpsc::channel();
        let mut kernel = voom_kernel::Kernel::new();
        kernel
            .register_plugin(Arc::new(RecordingExecutor { entered_tx }), 50)
            .unwrap();
        let ctx = ProcessContext {
            plan_limiter: Arc::new(limiter),
            ..fixture.make_ctx(
                Arc::new(kernel),
                Arc::new(voom_domain::test_support::InMemoryStore::new()),
            )
        };

        let processing = process_single_file_execute(&file, &compiled, &ctx);
        tokio::pin!(processing);

        wait_for_limiter_acquire(&mut wait_rx, "hw:nvenc", processing.as_mut()).await;
        assert!(
            matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "executor must not receive PlanCreated while the nvenc permit is held"
        );

        fixture.cancel();
        drop(held);

        tokio::time::timeout(Duration::from_secs(1), &mut processing)
            .await
            .expect("processing should stop after cancellation")
            .expect("processing should return without execution errors");
        assert!(
            matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "executor must not receive PlanCreated after cancellation"
        );
    }

    #[tokio::test]
    async fn process_pipeline_uses_phase_outputs_for_downstream_conditions() {
        let policy = r#"policy "test" {
                phase verify {
                    verify quick
                }

                phase after {
                    depends_on: [verify]
                    skip when verify.outcome == "ok"
                    set_tag "title" "should not execute"
                }
            }"#;
        let fixture = process_tests::TestFixture::with_policy(policy);
        let path = fixture.dir_path().join("movie.mkv");
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        let file = h264_file(path);
        let compiled = voom_dsl::compile_policy(policy).unwrap();

        let (executed_tx, executed_rx) = mpsc::channel();
        let mut kernel = voom_kernel::Kernel::new();
        kernel
            .register_plugin(Arc::new(VerifyOutcomeExecutor { executed_tx }), 50)
            .unwrap();
        let ctx = fixture.make_ctx(
            Arc::new(kernel),
            Arc::new(voom_domain::test_support::InMemoryStore::new()),
        );

        process_single_file_execute(&file, &compiled, &ctx)
            .await
            .expect("processing should complete");

        let executed: Vec<String> = executed_rx.try_iter().collect();
        assert_eq!(executed, vec!["verify"]);
    }
}
