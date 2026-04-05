//! Report Plugin — single source of truth for library statistics and snapshots.

pub mod query;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use anyhow::{Context, Result};
use voom_domain::events::{Event, EventResult};
use voom_domain::stats::SnapshotTrigger;
use voom_domain::storage::StorageTrait;
use voom_kernel::Plugin;

pub use query::{DatabaseStats, IssueReport, ReportRequest, ReportResult, ReportSection};

/// Report plugin — captures snapshots on lifecycle events and provides
/// a unified query interface for library statistics.
pub struct ReportPlugin {
    store: Arc<dyn StorageTrait>,
}

impl ReportPlugin {
    #[must_use]
    pub fn new(store: Arc<dyn StorageTrait>) -> Self {
        Self { store }
    }

    /// Query the library and assemble a report.
    ///
    /// Static method — callers pass the store directly so they don't
    /// need a plugin instance.
    pub fn query(store: &dyn StorageTrait, request: &ReportRequest) -> Result<ReportResult> {
        let mut result = ReportResult::default();

        if request.includes(ReportSection::Library) {
            let snapshot = store
                .gather_library_stats(SnapshotTrigger::Manual)
                .context("failed to gather library statistics")?;
            result.library = Some(snapshot);
        }

        if request.includes(ReportSection::Plans) {
            let stats = store
                .plan_stats_by_phase()
                .context("failed to query plan stats")?;
            result.plans = Some(stats);
        }

        if request.includes(ReportSection::Savings) {
            let report = store
                .savings_by_provenance(request.period)
                .context("failed to query savings")?;
            result.savings = Some(report);
        }

        if request.includes(ReportSection::History) {
            let limit = request.history_limit.unwrap_or(20);
            let snapshots = store
                .list_snapshots(limit)
                .context("failed to list snapshots")?;
            result.history = Some(snapshots);
        }

        if request.includes(ReportSection::Issues) {
            let files = store
                .list_files(&voom_domain::FileFilters::default())
                .context("failed to list files")?;
            let issues: Vec<query::IssueReport> = files
                .iter()
                .filter_map(|f| {
                    let violations_val = f.plugin_metadata.get("safeguard_violations")?;
                    let violations: Vec<voom_domain::SafeguardViolation> =
                        match serde_json::from_value(violations_val.clone()) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::warn!(
                                    path = %f.path.display(),
                                    error = %e,
                                    "malformed safeguard_violations metadata"
                                );
                                return None;
                            }
                        };
                    if violations.is_empty() {
                        return None;
                    }
                    Some(query::IssueReport {
                        path: f.path.clone(),
                        violations,
                    })
                })
                .collect();
            result.issues = Some(issues);
        }

        if request.includes(ReportSection::Database) {
            let table_counts = store
                .table_row_counts()
                .context("failed to query table row counts")?;
            let page_stats = store.page_stats().context("failed to query page stats")?;
            result.database = Some(query::DatabaseStats {
                table_counts,
                page_stats,
            });
        }

        Ok(result)
    }

    /// Capture and persist a library snapshot.
    pub fn capture_snapshot(
        store: &dyn StorageTrait,
        trigger: SnapshotTrigger,
    ) -> Result<voom_domain::stats::LibrarySnapshot> {
        let snapshot = store
            .gather_library_stats(trigger)
            .context("failed to gather library statistics")?;
        store
            .save_snapshot(&snapshot)
            .context("failed to save snapshot")?;
        Ok(snapshot)
    }

    fn handle_lifecycle_event(
        &self,
        trigger: SnapshotTrigger,
    ) -> voom_domain::errors::Result<Option<EventResult>> {
        match Self::capture_snapshot(self.store.as_ref(), trigger) {
            Ok(snapshot) => {
                tracing::info!(
                    trigger = %trigger,
                    files = snapshot.files.total_count,
                    "auto-captured library snapshot"
                );
                Ok(Some(EventResult::new("report")))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to auto-capture snapshot");
                Ok(None)
            }
        }
    }
}

impl Plugin for ReportPlugin {
    fn name(&self) -> &str {
        "report"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[voom_domain::Capability] {
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::SCAN_COMPLETE || event_type == Event::INTROSPECT_COMPLETE
    }

    fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
        match event {
            Event::ScanComplete(_) => self.handle_lifecycle_event(SnapshotTrigger::ScanComplete),
            // TODO(#120): no production code dispatches IntrospectComplete yet
            Event::IntrospectComplete(_) => {
                self.handle_lifecycle_event(SnapshotTrigger::IntrospectComplete)
            }
            _ => Ok(None),
        }
    }
}
