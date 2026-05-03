//! Retention runner. Reads per-table policies from `RetentionConfig`,
//! invokes each storage trait's `prune_old_*` method, and emits a
//! `RetentionCompletedEvent` on the kernel's event bus.

use std::sync::Arc;
use std::time::{Duration, Instant};

use voom_domain::events::{Event, RetentionCompletedEvent, RetentionTrigger, TableRetentionResult};
use voom_domain::storage::{PruneReport, RetentionPolicy};
use voom_kernel::Kernel;

use crate::config::{RetentionConfig, TableRetention};

/// Convert a TOML-loaded `TableRetention` into a `RetentionPolicy`.
///
/// `Some(0)` collapses to `None` so the policy treats it as "no bound".
#[must_use]
pub fn table_retention_to_policy(t: &TableRetention) -> RetentionPolicy {
    let max_age = t
        .keep_for_days
        .filter(|n| *n > 0)
        .map(|days| chrono::Duration::days(i64::from(days)));
    let keep_last = t.keep_last.filter(|n| *n > 0);
    RetentionPolicy { max_age, keep_last }
}

pub struct RetentionRunner {
    store: Arc<dyn voom_domain::storage::StorageTrait>,
    config: RetentionConfig,
    kernel: Option<Arc<Kernel>>,
}

#[derive(Debug)]
pub struct RetentionSummary {
    pub per_table: Vec<(String, anyhow::Result<PruneReport>)>,
    pub duration: Duration,
}

impl RetentionRunner {
    pub fn new(
        store: Arc<dyn voom_domain::storage::StorageTrait>,
        config: RetentionConfig,
        kernel: Option<Arc<Kernel>>,
    ) -> Self {
        Self {
            store,
            config,
            kernel,
        }
    }

    /// True when every configured table has both bounds disabled.
    #[must_use]
    pub fn is_fully_disabled(&self) -> bool {
        table_retention_to_policy(&self.config.jobs).is_disabled()
            && table_retention_to_policy(&self.config.event_log).is_disabled()
            && table_retention_to_policy(&self.config.file_transitions).is_disabled()
    }

    /// Run all three table prunes, log results, and emit `RetentionCompletedEvent`.
    pub fn run_once(&self, trigger: RetentionTrigger) -> RetentionSummary {
        let start = Instant::now();
        let per_table: Vec<(String, anyhow::Result<PruneReport>)> = vec![
            ("jobs".to_string(), self.prune_jobs()),
            ("event_log".to_string(), self.prune_event_log()),
            (
                "file_transitions".to_string(),
                self.prune_file_transitions(),
            ),
        ];

        let duration = start.elapsed();

        for (table, result) in &per_table {
            match result {
                Ok(r) => tracing::info!(
                    table = %table,
                    deleted = r.deleted,
                    kept = r.kept,
                    ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                    "retention complete"
                ),
                Err(e) => tracing::warn!(table = %table, error = %e, "retention failed"),
            }
        }

        if let Some(kernel) = &self.kernel {
            let event = Event::RetentionCompleted(RetentionCompletedEvent {
                trigger,
                per_table: per_table
                    .iter()
                    .map(|(t, r)| TableRetentionResult {
                        table: t.clone(),
                        deleted: r.as_ref().map(|x| x.deleted).unwrap_or(0),
                        kept: r.as_ref().map(|x| x.kept).unwrap_or(0),
                        error: r.as_ref().err().map(std::string::ToString::to_string),
                    })
                    .collect(),
                duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            });
            let _ = kernel.dispatch(event);
        }

        RetentionSummary {
            per_table,
            duration,
        }
    }

    fn prune_jobs(&self) -> anyhow::Result<PruneReport> {
        let policy = table_retention_to_policy(&self.config.jobs);
        Ok(self.store.prune_old_jobs(policy)?)
    }

    fn prune_event_log(&self) -> anyhow::Result<PruneReport> {
        let policy = table_retention_to_policy(&self.config.event_log);
        Ok(self.store.prune_old_event_log(policy)?)
    }

    fn prune_file_transitions(&self) -> anyhow::Result<PruneReport> {
        let policy = table_retention_to_policy(&self.config.file_transitions);
        Ok(self.store.prune_old_file_transitions(policy)?)
    }
}

/// Best-effort end-of-run prune. Returns whether retention ran.
/// Failures are logged but never propagate. No-op when `run_after_cli` is
/// false or when fully disabled.
pub fn maybe_run_after_cli(
    store: Arc<dyn voom_domain::storage::StorageTrait>,
    config: &RetentionConfig,
    kernel: Option<Arc<Kernel>>,
) {
    if !config.run_after_cli {
        return;
    }
    let runner = RetentionRunner::new(store, config.clone(), kernel);
    if runner.is_fully_disabled() {
        return;
    }
    let _ = runner.run_once(voom_domain::events::RetentionTrigger::CliEndOfRun);
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::test_support::InMemoryStore;

    #[test]
    fn table_retention_zero_disables_both_bounds() {
        let t = TableRetention {
            keep_for_days: Some(0),
            keep_last: Some(0),
        };
        let p = table_retention_to_policy(&t);
        assert!(p.max_age.is_none());
        assert!(p.keep_last.is_none());
        assert!(p.is_disabled());
    }

    #[test]
    fn table_retention_none_is_disabled() {
        let t = TableRetention {
            keep_for_days: None,
            keep_last: None,
        };
        assert!(table_retention_to_policy(&t).is_disabled());
    }

    #[test]
    fn run_once_returns_one_result_per_table() {
        let store: Arc<dyn voom_domain::storage::StorageTrait> = Arc::new(InMemoryStore::new());
        let runner = RetentionRunner::new(store, RetentionConfig::default(), None);
        let summary = runner.run_once(RetentionTrigger::OnDemand);
        assert_eq!(summary.per_table.len(), 3);
        assert!(summary.per_table.iter().all(|(_, r)| r.is_ok()));
    }

    #[test]
    fn is_fully_disabled_true_when_all_zero() {
        let zero = TableRetention {
            keep_for_days: Some(0),
            keep_last: Some(0),
        };
        let config = RetentionConfig {
            jobs: zero.clone(),
            event_log: zero.clone(),
            file_transitions: zero,
            ..RetentionConfig::default()
        };
        let store: Arc<dyn voom_domain::storage::StorageTrait> = Arc::new(InMemoryStore::new());
        let runner = RetentionRunner::new(store, config, None);
        assert!(runner.is_fully_disabled());
    }

    #[test]
    fn is_fully_disabled_false_when_any_enabled() {
        let store: Arc<dyn voom_domain::storage::StorageTrait> = Arc::new(InMemoryStore::new());
        let runner = RetentionRunner::new(store, RetentionConfig::default(), None);
        assert!(!runner.is_fully_disabled());
    }
}
