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
    if tctx.new_file.content_hash == tctx.old_file.content_hash
        && tctx.new_file.path == tctx.old_file.path
    {
        // Nothing changed — no transition to record, no rename, no hash refresh.
        return;
    }

    let mut transition = voom_domain::FileTransition::new(
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

    if tctx.old_file.path != tctx.new_file.path {
        transition = transition.with_from_path(tctx.old_file.path.clone());
    }

    let new_path = if tctx.old_file.path != tctx.new_file.path {
        Some(tctx.new_file.path.as_path())
    } else {
        None
    };
    let new_expected_hash = tctx.new_file.content_hash.as_deref();

    if let Err(e) = tctx.ctx.store.record_post_execution(
        &tctx.old_file.id,
        new_path,
        new_expected_hash,
        &transition,
    ) {
        tracing::warn!(
            path = %tctx.old_file.path.display(),
            error = %e,
            "failed to record post-execution bundle (rename + transition + expected_hash)"
        );
    }
}

/// Grouped arguments for `record_failure_transition`.
///
/// The `executor` should be the executor plugin name, or an empty string when
/// no executor was involved (e.g. safeguard abort).
pub(super) struct FailureTransitionContext<'a> {
    pub file: &'a voom_domain::media::MediaFile,
    pub plan: &'a voom_domain::plan::Plan,
    pub executor: &'a str,
    pub error_message: Option<&'a str>,
    pub ctx: &'a ProcessContext<'a>,
}

/// Record a failure transition in the store for a plan that did not succeed.
///
/// The file is unchanged on failure, so `to_size = from_size` and `to_hash =
/// from_hash`.
///
/// Dual-write: `file_transitions.error_message` is used for session-based
/// queries (`voom report errors`), while `plans.result` stores the structured
/// `ExecutionDetail` JSON for plan-based queries with full subprocess output.
pub(super) fn record_failure_transition(fctx: &FailureTransitionContext<'_>) {
    let file = fctx.file;
    let to_hash = file.content_hash.clone().unwrap_or_default();
    let mut transition = voom_domain::FileTransition::new(
        file.id,
        file.path.clone(),
        to_hash,
        file.size,
        voom_domain::TransitionSource::Voom,
    )
    .with_from(file.content_hash.clone(), Some(file.size))
    .with_detail(fctx.executor)
    .with_plan_id(fctx.plan.id)
    .with_processing(
        0,
        0,
        0,
        voom_domain::ProcessingOutcome::Failure,
        &fctx.plan.policy_name,
        &fctx.plan.phase_name,
    )
    .with_session_id(fctx.ctx.counters.session_id);

    if let Some(msg) = fctx.error_message {
        transition = transition.with_error_message(msg);
    }

    if let Err(e) = fctx.ctx.store.record_transition(&transition) {
        tracing::warn!(
            path = %fctx.file.path.display(),
            error = %e,
            "failed to record failure transition"
        );
    }
}
