use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::policy_map::PolicyResolver;

#[derive(Debug, Default)]
pub(super) struct PhaseStats {
    pub(super) completed: u64,
    pub(super) skipped: u64,
    pub(super) failed: u64,
    pub(super) skip_reasons: HashMap<String, u64>,
}

pub(super) type PhaseStatsMap = Arc<Mutex<HashMap<String, PhaseStats>>>;

pub(super) fn record_phase_stat(
    stats: &PhaseStatsMap,
    phase_name: &str,
    outcome: PhaseOutcomeKind,
) {
    let mut map = stats.lock();
    let entry = map.entry(phase_name.to_string()).or_default();
    match outcome {
        PhaseOutcomeKind::Completed => entry.completed += 1,
        PhaseOutcomeKind::Skipped(reason) => {
            entry.skipped += 1;
            *entry.skip_reasons.entry(reason).or_insert(0) += 1;
        }
        PhaseOutcomeKind::Failed => entry.failed += 1,
    }
}

pub(super) enum PhaseOutcomeKind {
    Completed,
    Skipped(String),
    Failed,
}

/// Shared mutable counters accumulated during a processing run.
///
/// `parking_lot::Mutex` is safe here because the lock is never held across
/// `.await` points; `phase_stats` is only locked inside synchronous closures.
#[derive(Clone)]
pub(super) struct RunCounters {
    pub(super) modified_count: Arc<AtomicU64>,
    pub(super) backup_bytes: Arc<AtomicU64>,
    pub(super) phase_stats: PhaseStatsMap,
    pub(super) plan_collector: Arc<Mutex<Vec<serde_json::Value>>>,
    pub(super) session_id: uuid::Uuid,
}

impl RunCounters {
    pub(super) fn new() -> Self {
        Self {
            modified_count: Arc::new(AtomicU64::new(0)),
            backup_bytes: Arc::new(AtomicU64::new(0)),
            phase_stats: Arc::new(Mutex::new(HashMap::new())),
            plan_collector: Arc::new(Mutex::new(Vec::new())),
            session_id: uuid::Uuid::new_v4(),
        }
    }
}

/// Shared context for processing a single file.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct ProcessContext<'a> {
    pub(super) resolver: &'a PolicyResolver,
    pub(super) kernel: Arc<voom_kernel::Kernel>,
    pub(super) store: Arc<dyn voom_domain::storage::StorageTrait>,
    pub(super) dry_run: bool,
    pub(super) plan_only: bool,
    pub(super) flag_size_increase: bool,
    pub(super) flag_duration_shrink: bool,
    /// When true, bypass the introspection cache and force a fresh ffprobe pass.
    pub(super) force_rescan: bool,
    pub(super) token: &'a CancellationToken,
    pub(super) ffprobe_path: Option<&'a str>,
    pub(super) capabilities: &'a voom_domain::CapabilityMap,
    pub(super) plan_limiter: Arc<voom_job_manager::plan_limiter::PlanExecutionLimiter>,
    pub(super) counters: &'a RunCounters,
}

/// Dependencies needed to persist file transitions.
pub(super) struct TransitionRecorder<'a> {
    pub(super) store: &'a dyn voom_domain::storage::StorageTrait,
    pub(super) session_id: uuid::Uuid,
}

/// Grouped arguments for `record_failure_transition`.
///
/// The `executor` should be the executor plugin name, or an empty string when
/// no executor was involved (e.g. safeguard abort).
pub(super) struct FailureTransitionContext<'a> {
    pub(super) file: &'a voom_domain::media::MediaFile,
    pub(super) plan: &'a voom_domain::plan::Plan,
    pub(super) executor: &'a str,
    pub(super) error_message: Option<&'a str>,
    pub(super) recorder: &'a TransitionRecorder<'a>,
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
    use std::sync::atomic::Ordering;

    use super::*;
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    use voom_domain::storage::FileTransitionStorage;
    use voom_domain::test_support::InMemoryStore;

    #[test]
    fn record_phase_stat_accumulates_outcomes_by_phase() {
        let counters = RunCounters::new();

        record_phase_stat(
            &counters.phase_stats,
            "metadata",
            PhaseOutcomeKind::Completed,
        );
        record_phase_stat(
            &counters.phase_stats,
            "metadata",
            PhaseOutcomeKind::Skipped("already tagged".to_string()),
        );
        record_phase_stat(
            &counters.phase_stats,
            "metadata",
            PhaseOutcomeKind::Skipped("already tagged".to_string()),
        );
        record_phase_stat(&counters.phase_stats, "verify", PhaseOutcomeKind::Failed);

        let stats = counters.phase_stats.lock();
        let metadata = stats.get("metadata").expect("metadata phase is recorded");
        assert_eq!(metadata.completed, 1);
        assert_eq!(metadata.skipped, 2);
        assert_eq!(metadata.failed, 0);
        assert_eq!(metadata.skip_reasons["already tagged"], 2);

        let verify = stats.get("verify").expect("verify phase is recorded");
        assert_eq!(verify.completed, 0);
        assert_eq!(verify.skipped, 0);
        assert_eq!(verify.failed, 1);
    }

    #[test]
    fn run_counters_start_empty_and_share_state_when_cloned() {
        let counters = RunCounters::new();
        let cloned = counters.clone();

        cloned.modified_count.fetch_add(1, Ordering::Relaxed);
        cloned.backup_bytes.fetch_add(42, Ordering::Relaxed);
        cloned
            .plan_collector
            .lock()
            .push(serde_json::json!({"phase": "metadata"}));

        assert_eq!(counters.modified_count.load(Ordering::Relaxed), 1);
        assert_eq!(counters.backup_bytes.load(Ordering::Relaxed), 42);
        assert_eq!(counters.plan_collector.lock().len(), 1);
    }

    #[test]
    fn record_failure_transition_persists_error_message() {
        let store = InMemoryStore::new();
        let mut file = MediaFile::new(std::path::PathBuf::from("/movies/example.mkv"));
        file.content_hash = Some("hash-a".to_string());
        file.size = 100;
        let plan = Plan::new(file.clone(), "policy", "verify");
        let recorder = TransitionRecorder {
            store: &store,
            session_id: uuid::Uuid::new_v4(),
        };

        record_failure_transition(&FailureTransitionContext {
            file: &file,
            plan: &plan,
            executor: "verifier",
            error_message: Some("verification failed"),
            recorder: &recorder,
        });

        let transitions = store
            .transitions_for_file(&file.id)
            .expect("transitions query");
        assert_eq!(transitions.len(), 1);
        assert_eq!(
            transitions[0].error_message.as_deref(),
            Some("verification failed")
        );
    }
}
