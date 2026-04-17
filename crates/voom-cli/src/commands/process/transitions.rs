//! File transition recording for the process pipeline.
//!
//! Collects the 10 distinct pieces of information needed to build a
//! `FileTransition` into a single context struct, matching the
//! `ProcessContext` / `RunResultsContext` pattern used elsewhere in the
//! module.

use super::ProcessContext;

/// Grouped arguments for `record_file_transition`.
pub(super) struct FileTransitionContext<'a> {
    pub old_file: &'a voom_domain::media::MediaFile,
    pub new_file: &'a voom_domain::media::MediaFile,
    pub executor: &'a str,
    pub elapsed_ms: u64,
    pub actions_taken: u32,
    pub tracks_modified: u32,
    pub policy_name: &'a str,
    pub phase_name: &'a str,
    pub plan_id: uuid::Uuid,
    pub ctx: &'a ProcessContext<'a>,
}

/// Record a file transition in the store if the content hash changed.
pub(super) fn record_file_transition(tctx: &FileTransitionContext<'_>) {
    if tctx.new_file.content_hash == tctx.old_file.content_hash {
        return;
    }
    let transition = voom_domain::FileTransition::new(
        tctx.old_file.id,
        tctx.new_file.path.clone(),
        tctx.new_file.content_hash.clone().unwrap_or_default(),
        tctx.new_file.size,
        voom_domain::TransitionSource::Voom,
    )
    .with_from(tctx.old_file.content_hash.clone(), Some(tctx.old_file.size))
    .with_detail(tctx.executor)
    .with_plan_id(tctx.plan_id)
    .with_processing(
        tctx.elapsed_ms,
        tctx.actions_taken,
        tctx.tracks_modified,
        voom_domain::ProcessingOutcome::Success,
        tctx.policy_name,
        tctx.phase_name,
    )
    .with_metadata_snapshot(voom_domain::MetadataSnapshot::from_media_file(
        tctx.new_file,
    ))
    .with_session_id(tctx.ctx.counters.session_id);

    if let Err(e) = tctx.ctx.store.record_transition(&transition) {
        tracing::warn!(error = %e, "failed to record transition");
    }

    if let Some(ref hash) = tctx.new_file.content_hash {
        if let Err(e) = tctx.ctx.store.update_expected_hash(&tctx.old_file.id, hash) {
            tracing::warn!(error = %e, "failed to update expected_hash");
        }
    }
}

/// Record a failure transition in the store for a plan that did not succeed.
///
/// The file is unchanged on failure, so `to_size = from_size` and `to_hash =
/// from_hash`. The `executor` argument should be the executor plugin name, or
/// an empty string when no executor was involved (e.g. size-increase abort).
///
/// Dual-write: `file_transitions.error_message` is used for session-based
/// queries (`voom report errors`), while `plans.result` stores the structured
/// `ExecutionDetail` JSON for plan-based queries with full subprocess output.
pub(super) fn record_failure_transition(
    file: &voom_domain::media::MediaFile,
    plan_id: uuid::Uuid,
    executor: &str,
    policy_name: &str,
    phase_name: &str,
    error_message: Option<&str>,
    ctx: &ProcessContext<'_>,
) {
    let to_hash = file.content_hash.clone().unwrap_or_default();
    let mut transition = voom_domain::FileTransition::new(
        file.id,
        file.path.clone(),
        to_hash,
        file.size,
        voom_domain::TransitionSource::Voom,
    )
    .with_from(file.content_hash.clone(), Some(file.size))
    .with_detail(executor)
    .with_plan_id(plan_id)
    .with_processing(
        0,
        0,
        0,
        voom_domain::ProcessingOutcome::Failure,
        policy_name,
        phase_name,
    )
    .with_session_id(ctx.counters.session_id);

    if let Some(msg) = error_message {
        transition = transition.with_error_message(msg);
    }

    if let Err(e) = ctx.store.record_transition(&transition) {
        tracing::warn!(error = %e, "failed to record failure transition");
    }
}
