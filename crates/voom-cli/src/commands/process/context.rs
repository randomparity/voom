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
    pub(super) plan_limiter: Arc<voom_job_manager::worker::PlanExecutionLimiter>,
    pub(super) counters: &'a RunCounters,
}
