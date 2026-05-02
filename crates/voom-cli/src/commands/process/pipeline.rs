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
use voom_domain::plan::OperationType;

use super::dispatch::PlanDispatcher;
use super::plan_outcome::PlanOutcome;
use super::safeguards::{
    annotate_disk_space_violations, check_disk_space, check_duration_shrink, check_size_increase,
    collect_safeguard_violations, dispatch_safeguard_violations,
};
use super::transitions::{
    record_failure_transition, record_file_transition, FailureTransitionContext,
    FileTransitionContext,
};
use super::{record_phase_stat, PhaseOutcomeKind, ProcessContext};

use crate::introspect::DiscoveredFilePayload;

pub(super) const AUDIO_LANGUAGE_DETECTOR_PLUGIN: &str = "audio-language-detector";

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

    let mut file = crate::introspect::introspect_file(
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
    // Runs on spawn_blocking because StorageTrait is synchronous rusqlite.
    let store = ctx.store.clone();
    let lookup_path = file.path.clone();
    let stored = tokio::task::spawn_blocking(move || store.file_by_path(&lookup_path))
        .await
        .map_err(|e| format!("file_by_path join error for {}: {e}", file.path.display()))?
        .inspect_err(|e| {
            tracing::warn!(
                path = %file.path.display(),
                error = %e,
                "failed to load stored file for plugin_metadata merge"
            );
        })
        .ok()
        .flatten();
    if let Some(stored) = stored {
        for (k, v) in stored.plugin_metadata {
            file.plugin_metadata.entry(k).or_insert(v);
        }
    }

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
        let plan = plan.with_session_id(ctx.counters.session_id);

        plans_evaluated += 1;

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
            continue;
        }

        // Execute this plan
        let plan_clone = plan.clone();
        let file_clone = current_file.clone();
        let kernel_clone = ctx.kernel.clone();
        let start = std::time::Instant::now();
        let exec_outcome = tokio::task::spawn_blocking(move || {
            execute_single_plan(&plan_clone, &file_clone, &kernel_clone)
        })
        .await
        .map_err(|e| format!("plan execution join error: {e}"))?;
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
                .await;
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
                });
                phase_outcomes.insert(
                    plan.phase_name.clone(),
                    voom_policy_evaluator::EvaluationOutcome::ExecutionFailed,
                );
                // Downstream phases still evaluate; run_if gates block
                // them via ExecutionFailed in phase_outcomes.
            }
        }
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
) -> voom_domain::media::MediaFile {
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

    // Update the existing row's path before re-introspection so the
    // `FileIntrospected` upsert merges into it instead of inserting a new
    // row at the post-execution path (preserves UUID and lineage).
    let post_exec_path = resolve_post_execution_path(file, std::slice::from_ref(&plan));
    if post_exec_path != file.path {
        if let Err(e) = ctx.store.rename_file_path(&file.id, &post_exec_path) {
            tracing::warn!(
                error = %e,
                old_path = %file.path.display(),
                new_path = %post_exec_path.display(),
                "failed to update files.path after path-changing execution"
            );
        }
    }

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
    });

    new_file
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
    match crate::introspect::introspect_file(
        current_path,
        size,
        hash,
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
            tracing::warn!(error = %e, "re-introspection failed, using previous state");
            file.clone()
        }
    }
}

/// Apply audio language detection results to track language fields.
///
/// If the `audio-language-detector` plugin has produced metadata, update
/// each track's language to match the detected value. This runs before
/// policy evaluation so that policies can filter on detected languages
/// (e.g. `remove audio where lang == zxx` for silent tracks).
pub(super) fn apply_detected_languages(file: &mut voom_domain::media::MediaFile) {
    let Some(metadata) = file.plugin_metadata.get(AUDIO_LANGUAGE_DETECTOR_PLUGIN) else {
        return;
    };

    let Some(detections) = metadata.get("detections").and_then(|d| d.as_array()) else {
        return;
    };

    for det in detections {
        let Some(track_index_u64) = det.get("track_index").and_then(serde_json::Value::as_u64)
        else {
            continue;
        };
        let track_index = u32::try_from(track_index_u64).unwrap_or(u32::MAX);
        let Some(detected) = det.get("detected_language").and_then(|v| v.as_str()) else {
            continue;
        };

        let Some(track) = file.tracks.iter_mut().find(|t| t.index == track_index) else {
            continue;
        };

        let Some(normalized) = voom_domain::utils::language::normalize_language(detected) else {
            tracing::warn!(
                path = %file.path.display(),
                track = track_index,
                detected = %detected,
                "unrecognized language code from detector, skipping"
            );
            continue;
        };

        if track.language == normalized {
            continue;
        }

        if track.language != "und" {
            tracing::warn!(
                path = %file.path.display(),
                track = track_index,
                existing = %track.language,
                detected = %normalized,
                "overwriting track language with detected value"
            );
        }

        track.language = normalized.to_string();
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
