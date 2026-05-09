//! File transition recording for the process pipeline.
//!
//! Collects the information needed to build a `FileTransition` without
//! requiring callers to pass the whole process orchestration context.

/// Dependencies needed to persist file transitions.
pub(super) struct TransitionRecorder<'a> {
    pub store: &'a dyn voom_domain::storage::StorageTrait,
    pub session_id: uuid::Uuid,
}

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
    pub recorder: &'a TransitionRecorder<'a>,
}

/// Record a file transition in the store if the content hash or path changed.
pub(super) fn record_file_transition(tctx: &FileTransitionContext<'_>) {
    let hash_changed = tctx.new_file.content_hash != tctx.old_file.content_hash;
    let path_changed = tctx.old_file.path != tctx.new_file.path;
    if !hash_changed && !path_changed {
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
    .with_session_id(tctx.recorder.session_id);

    if path_changed {
        transition = transition.with_from_path(tctx.old_file.path.clone());
    }

    let new_path = path_changed.then_some(tctx.new_file.path.as_path());
    // Only refresh `expected_hash` when the content actually changed —
    // otherwise we'd issue a no-op UPDATE on every path-only execution.
    let new_expected_hash = hash_changed
        .then_some(tctx.new_file.content_hash.as_deref())
        .flatten();

    if let Err(e) =
        tctx.recorder
            .store
            .record_post_execution(new_path, new_expected_hash, &transition)
    {
        tracing::warn!(
            path = %tctx.old_file.path.display(),
            error = %e,
            "failed to record post-execution bundle"
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
    pub recorder: &'a TransitionRecorder<'a>,
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
    .with_session_id(fctx.recorder.session_id);

    if let Some(msg) = fctx.error_message {
        transition = transition.with_error_message(msg);
    }

    if let Err(e) = fctx.recorder.store.record_transition(&transition) {
        tracing::warn!(
            path = %fctx.file.path.display(),
            error = %e,
            "failed to record failure transition"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    use voom_domain::storage::FileTransitionStorage;
    use voom_domain::test_support::InMemoryStore;

    use super::*;

    fn media_file(path: &str, hash: &str) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path));
        file.content_hash = Some(hash.to_string());
        file.size = 100;
        file
    }

    #[test]
    fn record_file_transition_skips_when_path_and_hash_are_unchanged() {
        let store = InMemoryStore::new();
        let file = media_file("/movies/example.mkv", "hash-a");
        let recorder = TransitionRecorder {
            store: &store,
            session_id: uuid::Uuid::new_v4(),
        };
        let plan_id = uuid::Uuid::new_v4();

        record_file_transition(&FileTransitionContext {
            old_file: &file,
            new_file: &file,
            executor: "ffmpeg-executor",
            elapsed_ms: 10,
            actions_taken: 1,
            tracks_modified: 1,
            policy_name: "policy",
            phase_name: "metadata",
            plan_id,
            recorder: &recorder,
        });

        assert!(store
            .transitions_for_file(&file.id)
            .expect("transitions query")
            .is_empty());
    }

    #[test]
    fn record_file_transition_persists_path_and_processing_metadata() {
        let store = InMemoryStore::new();
        let old_file = media_file("/movies/example.mp4", "hash-a");
        let mut new_file = old_file.clone();
        new_file.path = PathBuf::from("/movies/example.mkv");
        new_file.content_hash = Some("hash-b".to_string());
        new_file.size = 120;
        let recorder = TransitionRecorder {
            store: &store,
            session_id: uuid::Uuid::new_v4(),
        };
        let plan_id = uuid::Uuid::new_v4();

        record_file_transition(&FileTransitionContext {
            old_file: &old_file,
            new_file: &new_file,
            executor: "ffmpeg-executor",
            elapsed_ms: 25,
            actions_taken: 2,
            tracks_modified: 1,
            policy_name: "policy",
            phase_name: "container",
            plan_id,
            recorder: &recorder,
        });

        let transitions = store
            .transitions_for_file(&old_file.id)
            .expect("transitions query");
        assert_eq!(transitions.len(), 1);
        let transition = &transitions[0];
        assert_eq!(transition.path, new_file.path);
        assert_eq!(
            transition.from_path.as_deref(),
            Some(old_file.path.as_path())
        );
        assert_eq!(transition.plan_id, Some(plan_id));
        assert_eq!(transition.source_detail.as_deref(), Some("ffmpeg-executor"));
        assert_eq!(transition.duration_ms, Some(25));
        assert_eq!(transition.actions_taken, Some(2));
        assert_eq!(transition.tracks_modified, Some(1));
    }

    #[test]
    fn record_failure_transition_persists_error_message() {
        let store = InMemoryStore::new();
        let file = media_file("/movies/example.mkv", "hash-a");
        let plan = Plan::new(file.clone(), "policy", "metadata");
        let recorder = TransitionRecorder {
            store: &store,
            session_id: uuid::Uuid::new_v4(),
        };

        record_failure_transition(&FailureTransitionContext {
            file: &file,
            plan: &plan,
            executor: "ffmpeg-executor",
            error_message: Some("bad codec"),
            recorder: &recorder,
        });

        let transitions = store
            .transitions_for_file(&file.id)
            .expect("transitions query");
        assert_eq!(transitions.len(), 1);
        let transition = &transitions[0];
        assert_eq!(transition.error_message.as_deref(), Some("bad codec"));
        assert_eq!(transition.source_detail.as_deref(), Some("ffmpeg-executor"));
        assert_eq!(transition.plan_id, Some(plan.id));
    }
}
