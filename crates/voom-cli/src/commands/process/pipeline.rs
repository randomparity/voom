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
use std::path::Path;
use std::sync::atomic::Ordering as AtomicOrdering;

use anyhow::Context;
use voom_domain::events::PlanFailedEvent;
use voom_domain::media::{CropDetection, MediaFile, TrackType};
use voom_domain::plan::{ActionParams, CropSettings, OperationType, Plan};
use voom_ffmpeg_executor::cropdetect::CropDetectSource;

use super::audio_language::apply_detected_languages;
use super::dispatch::PlanDispatcher;
use super::plan_outcome::PlanOutcome;
use super::safeguards::{
    annotate_disk_space_violations, check_disk_space, check_duration_shrink, check_size_increase,
    collect_safeguard_violations, dispatch_safeguard_violations,
};
use super::transitions::{
    FailureTransitionContext, FileTransitionContext, record_failure_transition,
    record_file_transition,
};
use super::{PhaseOutcomeKind, ProcessContext, record_phase_stat};

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
            ctx.animation_detection_mode,
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

    collect_safeguard_violations(file, &result, ctx);

    let needs_exec = voom_phase_orchestrator::needs_execution(&result);
    if needs_exec {
        ctx.counters
            .modified_count
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

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

    if ctx.plan_only {
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

    if ctx.estimate_mode {
        ctx.counters
            .estimate_plans
            .lock()
            .extend(result.plans.iter().cloned());
    }

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
    let keep_backups = compiled.config.keep_backups;

    // Verify file hasn't changed since introspection (TOCTOU guard)
    if let Some(skip_json) = check_file_hash(file).await {
        return Ok(Some(skip_json));
    }

    let mut current_file = file.clone();
    let mut phase_outcomes: HashMap<String, voom_policy_evaluator::EvaluationOutcome> =
        HashMap::new();
    let mut any_executed = false;
    let mut modified_counted = false;
    let mut plans_evaluated: usize = 0;
    let mut failed_plan_count: usize = 0;

    for phase_name in &compiled.phase_order {
        if ctx.token.is_cancelled() {
            break;
        }

        let Some(plan) = voom_policy_evaluator::evaluate_single_phase_with_hints(
            phase_name,
            compiled,
            &current_file,
            &phase_outcomes,
            ctx.capabilities,
        ) else {
            continue;
        };
        let mut plan = plan
            .with_session_id(ctx.counters.session_id)
            .with_scan_session(ctx.scan_session);

        plans_evaluated += 1;

        if savings_below_threshold(&plan, ctx) {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::Skipped,
            );
            record_phase_stat(
                &ctx.counters.phase_stats,
                &plan.phase_name,
                PhaseOutcomeKind::Skipped("estimated savings below threshold".to_string()),
            );
            continue;
        }

        dispatch_safeguard_violations(&plan, &current_file, ctx);

        // Handle skipped plans
        if let Some(reason) = &plan.skip_reason {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::Skipped,
            );
            dispatch_skipped_plan(&plan, &current_file, reason, ctx);
            continue;
        }

        // Empty plans need no execution
        if plan.is_empty() {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::Executed { modified: false },
            );
            continue;
        }

        let mut permit = None;
        if plan_requires_auto_crop(&plan) {
            let acquired = tokio::select! {
                biased;
                () = ctx.token.cancelled() => break,
                permit = ctx.plan_limiter.acquire_for_plan(&plan) => permit,
            };
            if let Err(error) = apply_auto_crop_detection(&mut plan, &mut current_file, ctx).await {
                let mut failed = PlanFailedEvent::new(
                    plan.id,
                    current_file.path.clone(),
                    plan.phase_name.clone(),
                    error,
                );
                failed.plugin_name = Some("ffmpeg-executor".to_string());
                dispatch_plan_failure(failed, &plan.phase_name, ctx);
                phase_outcomes.insert(
                    phase_name.clone(),
                    voom_policy_evaluator::EvaluationOutcome::ExecutionFailed,
                );
                failed_plan_count += 1;
                drop(acquired);
                continue;
            }
            permit = Some(acquired);
        }

        // Pre-execution safeguard: check disk space
        // Note: check_disk_space dispatches only PlanFailed (not
        // PlanCreated, which would trigger executors) and records
        // PhaseOutcomeKind::Failed for stats. This insert updates
        // the dependency-resolution map to block downstream run_if gates.
        if check_disk_space(&plan, &current_file, ctx) {
            phase_outcomes.insert(
                phase_name.clone(),
                voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
            );
            failed_plan_count += 1;
            continue;
        }

        // Execute this plan
        let permit = match permit {
            Some(permit) => permit,
            None => {
                tokio::select! {
                    biased;
                    () = ctx.token.cancelled() => break,
                    permit = ctx.plan_limiter.acquire_for_plan(&plan) => permit,
                }
            }
        };
        let plan_clone = plan.clone();
        let file_clone = current_file.clone();
        let kernel_clone = ctx.kernel.clone();
        let start = std::time::Instant::now();
        let exec_outcome = tokio::task::spawn_blocking(move || {
            execute_single_plan(&plan_clone, &file_clone, &kernel_clone)
        })
        .await
        .map_err(|e| format!("plan execution join error: {e}"))?;
        drop(permit);
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        match exec_outcome {
            PlanOutcome::Success { executor } => {
                // Post-execution safeguard: check size increase
                // Note: check_size_increase dispatches PlanFailed (PlanCreated was
                // already dispatched by execute_single_plan) and records
                // PhaseOutcomeKind::Failed for stats. This insert updates the
                // dependency-resolution map.
                if check_size_increase(&plan, &current_file, ctx) {
                    phase_outcomes.insert(
                        phase_name.clone(),
                        voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
                    );
                    failed_plan_count += 1;
                    continue;
                }
                // Post-execution safeguard: check duration shrinkage.
                // Mirrors check_size_increase: dispatches PlanFailed
                // (PlanCreated was already dispatched by execute_single_plan)
                // and records PhaseOutcomeKind::Failed for stats.
                if check_duration_shrink(&plan, &current_file, ctx).await {
                    phase_outcomes.insert(
                        phase_name.clone(),
                        voom_policy_evaluator::EvaluationOutcome::SafeguardFailed,
                    );
                    failed_plan_count += 1;
                    continue;
                }
                any_executed = true;
                if !modified_counted {
                    ctx.counters
                        .modified_count
                        .fetch_add(1, AtomicOrdering::Relaxed);
                    modified_counted = true;
                }
                phase_outcomes.insert(
                    phase_name.clone(),
                    voom_policy_evaluator::EvaluationOutcome::Executed { modified: true },
                );
                current_file = handle_plan_success(
                    plan,
                    &current_file,
                    &executor,
                    elapsed_ms,
                    keep_backups,
                    ctx,
                )
                .await
                .map_err(|error| error.to_string())?;
            }
            PlanOutcome::Failed(failed) => {
                let executor = failed.plugin_name.clone().unwrap_or_default();
                let error_msg = failed.error.clone();
                dispatch_plan_failure(failed, &plan.phase_name, ctx);
                record_failure_transition(&FailureTransitionContext {
                    file: &current_file,
                    plan: &plan,
                    executor: &executor,
                    error_message: Some(&error_msg),
                    ctx,
                })
                .map_err(|error| error.to_string())?;
                phase_outcomes.insert(
                    plan.phase_name.clone(),
                    voom_policy_evaluator::EvaluationOutcome::ExecutionFailed,
                );
                failed_plan_count += 1;
                // Downstream phases still evaluate; run_if gates block
                // them via ExecutionFailed in phase_outcomes.
            }
        }
    }

    if failed_plan_count > 0 {
        return Err(format!(
            "{failed_plan_count} executable plan(s) failed for {file_path_str}"
        ));
    }

    Ok(Some(serde_json::json!({
        "path": file_path_str,
        "needs_execution": any_executed,
        "plans_evaluated": plans_evaluated,
    })))
}

/// Dispatch events for a skipped plan: `PlanCreated` then `PlanSkipped`.
fn dispatch_skipped_plan(
    plan: &voom_domain::plan::Plan,
    file: &voom_domain::media::MediaFile,
    reason: &str,
    ctx: &ProcessContext<'_>,
) {
    let dispatcher = PlanDispatcher::new(&ctx.kernel);
    dispatcher.created(plan);
    dispatcher.skipped(plan, file, reason);
    record_phase_stat(
        &ctx.counters.phase_stats,
        &plan.phase_name,
        PhaseOutcomeKind::Skipped(reason.to_string()),
    );
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
pub(super) async fn handle_plan_success(
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
        ctx,
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
    let introspection_result = crate::introspect::introspect_file_no_dispatch(
        current_path.clone(),
        size,
        hash.clone(),
        &kernel_clone,
        ffp.as_deref(),
        ctx.animation_detection_mode,
    )
    .await;
    match introspection_result {
        Ok(mut new_file) => {
            preserve_persisted_file_identity(file, &mut new_file);
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

/// Keep the database identity of the row being advanced through the process
/// pipeline. Re-introspection builds a fresh `MediaFile`, but downstream
/// phases must continue to reference the persisted row that
/// `record_post_execution` just renamed/updated.
fn preserve_persisted_file_identity(
    persisted_file: &voom_domain::media::MediaFile,
    reintrospected_file: &mut voom_domain::media::MediaFile,
) {
    reintrospected_file.id = persisted_file.id;
    reintrospected_file.expected_hash = persisted_file.expected_hash.clone();
    reintrospected_file.status = persisted_file.status;
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
    let plans =
        voom_policy_evaluator::evaluate_with_capabilities(compiled, file, capabilities).plans;
    voom_phase_orchestrator::orchestrate(plans)
}

/// Determine the file path after plan execution.
///
/// If a `ConvertContainer` action changed the container, the file extension
/// will have changed on disk (e.g. `.mp4` → `.mkv`). Derive the new path
/// from the plan actions; fall back to the original path if unchanged.
pub(super) fn resolve_post_execution_path(
    file: &voom_domain::media::MediaFile,
    plans: &[voom_domain::plan::Plan],
) -> std::path::PathBuf {
    if let Some(container) = find_last_container_action(plans) {
        let new_path = file.path.with_extension(container.as_str());
        if new_path.exists() {
            return new_path;
        }
    }
    file.path.clone()
}

/// Search plans (last to first) for the most recent `ConvertContainer` action.
fn find_last_container_action(
    plans: &[voom_domain::plan::Plan],
) -> Option<voom_domain::media::Container> {
    for plan in plans.iter().rev() {
        if plan.is_skipped() || plan.is_empty() {
            continue;
        }
        for action in &plan.actions {
            if action.operation == OperationType::ConvertContainer {
                if let voom_domain::plan::ActionParams::Container { container } = &action.parameters
                {
                    return Some(*container);
                }
            }
        }
    }
    None
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

type CropDetector =
    fn(&str, &Path, CropDetectSource, &CropSettings) -> voom_domain::Result<Option<CropDetection>>;

struct CropDetectRequest {
    settings: CropSettings,
    source: CropDetectSource,
}

async fn apply_auto_crop_detection(
    plan: &mut Plan,
    current_file: &mut MediaFile,
    ctx: &ProcessContext<'_>,
) -> Result<(), String> {
    apply_auto_crop_detection_with_detector(
        plan,
        current_file,
        ctx,
        voom_ffmpeg_executor::cropdetect::detect_crop,
    )
    .await
}

async fn apply_auto_crop_detection_with_detector(
    plan: &mut Plan,
    current_file: &mut MediaFile,
    ctx: &ProcessContext<'_>,
    detector: CropDetector,
) -> Result<(), String> {
    let Some(request) = cropdetect_request_for_plan(plan, current_file) else {
        return Ok(());
    };
    let settings_fingerprint = crop_settings_fingerprint(&request.settings)?;
    if current_file
        .crop_detection
        .as_ref()
        .and_then(|detection| detection.settings_fingerprint.as_deref())
        == Some(settings_fingerprint.as_str())
    {
        plan.file = current_file.clone();
        return Ok(());
    }

    let ffmpeg_path = "ffmpeg".to_string();
    let source_path = current_file.path.clone();
    let settings = request.settings;
    let source = request.source;
    let detection = tokio::task::spawn_blocking(move || {
        detector(&ffmpeg_path, &source_path, source, &settings)
    })
    .await
    .map_err(|e| format!("crop detection join error: {e}"))?
    .map_err(|e| format!("crop detection failed: {e}"))?
    .map(|detection| detection.with_settings_fingerprint(settings_fingerprint));

    let changed = current_file.crop_detection != detection;
    if changed {
        let mut updated_file = current_file.clone();
        updated_file.crop_detection = detection;
        ctx.store
            .upsert_file(&updated_file)
            .map_err(|e| format!("failed to persist crop detection: {e}"))?;
        *current_file = updated_file;
        plan.file = current_file.clone();
    }
    Ok(())
}

fn plan_requires_auto_crop(plan: &Plan) -> bool {
    plan.actions.iter().any(|action| {
        action.operation == OperationType::TranscodeVideo
            && matches!(
                &action.parameters,
                ActionParams::Transcode { settings, .. } if settings.crop.is_some()
            )
    })
}

fn cropdetect_request_for_plan(plan: &Plan, file: &MediaFile) -> Option<CropDetectRequest> {
    let action = plan.actions.iter().find(|action| {
        action.operation == OperationType::TranscodeVideo
            && matches!(
                &action.parameters,
                ActionParams::Transcode { settings, .. } if settings.crop.is_some()
            )
    })?;
    let ActionParams::Transcode { settings, .. } = &action.parameters else {
        return None;
    };
    let crop_settings = settings.crop.clone()?;
    let track_index = action.track_index?;
    let track = file
        .tracks
        .iter()
        .find(|track| track.index == track_index && track.track_type == TrackType::Video)?;
    let width = track.width?;
    let height = track.height?;
    Some(CropDetectRequest {
        settings: crop_settings,
        source: CropDetectSource::new(width, height, file.duration),
    })
}

fn crop_settings_fingerprint(settings: &CropSettings) -> Result<String, String> {
    serde_json::to_string(settings).map_err(|e| format!("failed to fingerprint crop settings: {e}"))
}

fn savings_below_threshold(plan: &Plan, ctx: &ProcessContext<'_>) -> bool {
    let Some(threshold) = ctx.confirm_savings else {
        return false;
    };
    let estimate = voom_domain::estimate_plans(
        voom_domain::EstimateInput::new(vec![plan.clone()], 1, chrono::Utc::now()),
        &ctx.estimate_model,
    );
    estimate.bytes_saved < threshold as i64
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use voom_domain::capabilities::Capability;
    use voom_domain::errors::VoomError;
    use voom_domain::events::{Event, EventResult};
    use voom_domain::media::{Container, CropDetection, CropRect, MediaFile, Track, TrackType};
    use voom_domain::plan::{CropSettings, PlannedAction, TranscodeSettings};
    use voom_domain::storage::FileStorage;

    use super::super::tests as process_tests;
    use super::*;

    struct RecordingExecutor {
        entered_tx: mpsc::Sender<()>,
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

    struct FailingExecutor;

    impl voom_kernel::Plugin for FailingExecutor {
        fn name(&self) -> &'static str {
            "failing-executor"
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
                return Ok(Some(EventResult::plan_failed(
                    self.name(),
                    "simulated executor failure",
                )));
            }
            Ok(None)
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
        let mut file = MediaFile::new(path)
            .with_container(Container::Mkv)
            .with_tracks(vec![Track::new(0, TrackType::Video, "h264".into())]);
        file.duration = 120.0;
        file.tracks[0].width = Some(1920);
        file.tracks[0].height = Some(1080);
        file
    }

    fn crop_plan(file: MediaFile, settings: CropSettings) -> Plan {
        let mut plan = Plan::new(file, "test-policy", "transcode-video");
        plan.actions = vec![PlannedAction::track_op(
            OperationType::TranscodeVideo,
            0,
            ActionParams::Transcode {
                codec: "hevc".into(),
                settings: TranscodeSettings::default().with_crop(Some(settings)),
            },
            "Transcode video with crop",
        )];
        plan
    }

    fn crop_detector_ok(
        _ffmpeg_path: &str,
        _source_path: &Path,
        _source: CropDetectSource,
        _settings: &CropSettings,
    ) -> voom_domain::Result<Option<CropDetection>> {
        Ok(Some(CropDetection::new(
            CropRect::new(0, 132, 0, 132),
            chrono::Utc::now(),
        )))
    }

    fn crop_detector_fails(
        _ffmpeg_path: &str,
        _source_path: &Path,
        _source: CropDetectSource,
        _settings: &CropSettings,
    ) -> voom_domain::Result<Option<CropDetection>> {
        Err(VoomError::ToolExecution {
            tool: "ffmpeg".to_string(),
            message: "synthetic cropdetect failure".to_string(),
        })
    }

    async fn held_nvenc_permit(
        limiter: &voom_job_manager::worker::PlanExecutionLimiter,
    ) -> voom_job_manager::worker::PlanExecutionPermit {
        limiter
            .acquire_for_plan(&process_tests::test_plan_with_transcode_hw(
                "transcode-video",
                "nvenc",
            ))
            .await
    }

    async fn assert_executor_not_entered_while_limited(
        entered_rx: &mpsc::Receiver<()>,
        processing: Pin<
            &mut impl Future<Output = std::result::Result<Option<serde_json::Value>, String>>,
        >,
        message: &str,
    ) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            result = processing => {
                panic!("processing finished before the limited executor was released: {result:?}");
            }
        }
        assert!(
            matches!(entered_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "{message}"
        );
    }

    #[tokio::test]
    async fn auto_crop_detection_persists_and_updates_plan_file() {
        let fixture = process_tests::TestFixture::with_policy(global_hw_policy());
        let store = Arc::new(voom_domain::test_support::InMemoryStore::new());
        let ctx = fixture.make_ctx(Arc::new(voom_kernel::Kernel::new()), store.clone());
        let path = fixture.dir_path().join("movie.mkv");
        let mut file = h264_file(path);
        let mut plan = crop_plan(file.clone(), CropSettings::auto());

        apply_auto_crop_detection_with_detector(&mut plan, &mut file, &ctx, crop_detector_ok)
            .await
            .unwrap();

        let detection = file.crop_detection.as_ref().expect("crop detection");
        assert_eq!(detection.rect, CropRect::new(0, 132, 0, 132));
        assert!(detection.settings_fingerprint.is_some());
        assert_eq!(
            plan.file.crop_detection.as_ref().map(|d| d.rect),
            Some(CropRect::new(0, 132, 0, 132))
        );
        let stored = store.file(&file.id).unwrap().expect("stored file");
        assert_eq!(
            stored.crop_detection.as_ref().map(|d| d.rect),
            Some(CropRect::new(0, 132, 0, 132))
        );
    }

    #[tokio::test]
    async fn auto_crop_detection_reuses_matching_cached_fingerprint() {
        let fixture = process_tests::TestFixture::with_policy(global_hw_policy());
        let ctx = fixture.make_default_ctx();
        let path = fixture.dir_path().join("movie.mkv");
        let settings = CropSettings::auto();
        let fingerprint = crop_settings_fingerprint(&settings).unwrap();
        let mut file = h264_file(path);
        file.crop_detection = Some(
            CropDetection::new(CropRect::new(0, 120, 0, 120), chrono::Utc::now())
                .with_settings_fingerprint(fingerprint),
        );
        let mut plan = crop_plan(file.clone(), settings);

        apply_auto_crop_detection_with_detector(&mut plan, &mut file, &ctx, crop_detector_fails)
            .await
            .unwrap();

        assert_eq!(
            file.crop_detection.as_ref().map(|d| d.rect),
            Some(CropRect::new(0, 120, 0, 120))
        );
    }

    #[tokio::test]
    async fn auto_crop_detection_failure_leaves_file_unmodified() {
        let fixture = process_tests::TestFixture::with_policy(global_hw_policy());
        let ctx = fixture.make_default_ctx();
        let path = fixture.dir_path().join("movie.mkv");
        let mut file = h264_file(path);
        let mut plan = crop_plan(file.clone(), CropSettings::auto());

        let err = apply_auto_crop_detection_with_detector(
            &mut plan,
            &mut file,
            &ctx,
            crop_detector_fails,
        )
        .await
        .unwrap_err();

        assert!(err.contains("crop detection failed"));
        assert!(file.crop_detection.is_none());
        assert!(plan.file.crop_detection.is_none());
    }

    #[test]
    fn plan_requires_auto_crop_only_for_crop_enabled_transcode() {
        let path = std::path::PathBuf::from("/tmp/movie.mkv");
        let file = h264_file(path);
        let crop = crop_plan(file.clone(), CropSettings::auto());
        let plain = process_tests::test_plan_with_optional_transcode_hw("transcode-video", None);

        assert!(plan_requires_auto_crop(&crop));
        assert!(!plan_requires_auto_crop(&plain));
    }

    #[test]
    fn reintrospection_preserves_persisted_file_identity_for_downstream_phases() {
        let mut persisted = h264_file(std::path::PathBuf::from("/library/movie.mp4"));
        persisted.expected_hash = Some("previous-expected-hash".to_string());
        persisted.status = voom_domain::transition::FileStatus::Active;

        let fresh_id = uuid::Uuid::new_v4();
        let mut reintrospected = h264_file(std::path::PathBuf::from("/library/movie.mkv"));
        reintrospected.id = fresh_id;
        reintrospected.content_hash = Some("post-execution-hash".to_string());
        reintrospected.size = 2048;

        preserve_persisted_file_identity(&persisted, &mut reintrospected);

        assert_eq!(
            reintrospected.id, persisted.id,
            "downstream plans must keep the persisted files.id, not the fresh ffprobe UUID"
        );
        assert_ne!(
            reintrospected.id, fresh_id,
            "the transient re-introspection UUID must not escape the phase boundary"
        );
        assert_eq!(
            reintrospected.path,
            std::path::PathBuf::from("/library/movie.mkv"),
            "post-execution metadata must still reflect the re-introspected path"
        );
        assert_eq!(
            reintrospected.content_hash.as_deref(),
            Some("post-execution-hash"),
            "post-execution content metadata must come from re-introspection"
        );
        assert_eq!(
            reintrospected.expected_hash.as_deref(),
            Some("previous-expected-hash"),
            "identity preservation must not invent expected_hash updates before storage commits"
        );
    }

    #[tokio::test]
    async fn failed_executable_plan_returns_file_job_error() {
        let policy = global_hw_policy();
        let fixture = process_tests::TestFixture::with_policy(policy);
        let path = fixture.dir_path().join("movie.mkv");
        let file = h264_file(path);
        let compiled = voom_dsl::compile_policy(policy).unwrap();

        let mut kernel = voom_kernel::Kernel::new();
        kernel
            .register_plugin(Arc::new(FailingExecutor), 50)
            .unwrap();
        let ctx = fixture.make_ctx(
            Arc::new(kernel),
            Arc::new(voom_domain::test_support::InMemoryStore::new()),
        );

        let error = process_single_file_execute(&file, &compiled, &ctx)
            .await
            .expect_err("failed executable plans must fail the file job");

        assert!(
            error.contains("1 executable plan(s) failed"),
            "unexpected error: {error}"
        );
        let stats = ctx.counters.phase_stats.lock();
        let phase = stats
            .get("transcode-video")
            .expect("failed phase should be recorded");
        assert_eq!(phase.failed, 1);
        assert_eq!(phase.completed, 0);
    }

    #[tokio::test]
    async fn process_pipeline_respects_plan_limiter() {
        let policy = nvenc_policy();
        let fixture = process_tests::TestFixture::with_policy(policy);
        let limiter = voom_job_manager::worker::PlanExecutionLimiter::from_limits(vec![(
            "hw:nvenc".to_string(),
            1,
        )]);
        let held = held_nvenc_permit(&limiter).await;

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

        assert_executor_not_entered_while_limited(
            &entered_rx,
            processing.as_mut(),
            "executor must not receive PlanCreated while the nvenc permit is held",
        )
        .await;

        drop(held);
        tokio::time::timeout(Duration::from_secs(5), &mut processing)
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
        let limiter = voom_job_manager::worker::PlanExecutionLimiter::from_limits_with_default(
            vec![("hw:nvenc".to_string(), 1)],
            Some("hw:nvenc".to_string()),
        );
        let held = limiter
            .acquire_for_plan(&process_tests::test_plan_with_optional_transcode_hw(
                "transcode-video",
                None,
            ))
            .await;

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

        assert_executor_not_entered_while_limited(
            &entered_rx,
            processing.as_mut(),
            "executor must not receive PlanCreated while the default nvenc permit is held",
        )
        .await;

        drop(held);
        tokio::time::timeout(Duration::from_secs(5), &mut processing)
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
        let limiter = voom_job_manager::worker::PlanExecutionLimiter::from_limits(vec![(
            "hw:nvenc".to_string(),
            1,
        )]);
        let held = held_nvenc_permit(&limiter).await;

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

        assert_executor_not_entered_while_limited(
            &entered_rx,
            processing.as_mut(),
            "executor must not receive PlanCreated while the nvenc permit is held",
        )
        .await;

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
}
