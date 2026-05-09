//! Report Plugin — single source of truth for library statistics and snapshots.

pub mod query;

use std::sync::Arc;

use voom_domain::errors::{Result, VoomError};
use voom_domain::events::{Event, EventResult};
use voom_domain::stats::SnapshotTrigger;
use voom_domain::storage::StorageTrait;
use voom_domain::Capability;
use voom_kernel::Plugin;

pub use query::{DatabaseStats, IssueReport, ReportRequest, ReportResult, ReportSection};

/// Create a `VoomError::Plugin` for the report plugin that preserves the
/// underlying error's display in its message.
pub(crate) fn plugin_err(context: &str, err: impl std::fmt::Display) -> VoomError {
    VoomError::plugin("report", format!("{context}: {err}"))
}

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
        query::assemble_report(store, request)
    }

    /// Capture and persist a library snapshot.
    pub fn capture_snapshot(
        store: &dyn StorageTrait,
        trigger: SnapshotTrigger,
    ) -> Result<voom_domain::stats::LibrarySnapshot> {
        let snapshot = store
            .gather_library_stats(trigger)
            .map_err(|e| plugin_err("failed to gather library statistics", e))?;
        store
            .save_snapshot(&snapshot)
            .map_err(|e| plugin_err("failed to save snapshot", e))?;
        Ok(snapshot)
    }

    fn handle_lifecycle_event(&self, trigger: SnapshotTrigger) -> Option<EventResult> {
        match Self::capture_snapshot(self.store.as_ref(), trigger) {
            Ok(snapshot) => {
                tracing::info!(
                    trigger = %trigger,
                    files = snapshot.files.total_count,
                    "auto-captured library snapshot"
                );
                Some(EventResult::new("report"))
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to auto-capture snapshot");
                None
            }
        }
    }
}

impl Plugin for ReportPlugin {
    fn name(&self) -> &'static str {
        "report"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    voom_kernel::plugin_cargo_metadata!();

    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == Event::SCAN_COMPLETE || event_type == Event::INTROSPECT_SESSION_COMPLETED
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::ScanComplete(_) => {
                Ok(self.handle_lifecycle_event(SnapshotTrigger::ScanComplete))
            }
            Event::IntrospectSessionCompleted(_) => {
                Ok(self.handle_lifecycle_event(SnapshotTrigger::IntrospectComplete))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::query::{ReportRequest, ReportSection};

    #[test]
    fn request_includes_explicit_sections() {
        let req = ReportRequest::new(vec![ReportSection::Library, ReportSection::Plans]);
        assert!(req.includes(ReportSection::Library));
        assert!(req.includes(ReportSection::Plans));
        assert!(!req.includes(ReportSection::Savings));
        assert!(!req.includes(ReportSection::History));
        assert!(!req.includes(ReportSection::Issues));
        assert!(!req.includes(ReportSection::Database));
    }

    #[test]
    fn request_all_includes_everything() {
        let req = ReportRequest::all();
        assert!(req.includes(ReportSection::Library));
        assert!(req.includes(ReportSection::Plans));
        assert!(req.includes(ReportSection::Savings));
        assert!(req.includes(ReportSection::History));
        assert!(req.includes(ReportSection::Issues));
        assert!(req.includes(ReportSection::Database));
    }

    #[test]
    fn request_summary_includes_only_library() {
        let req = ReportRequest::summary();
        assert!(req.includes(ReportSection::Library));
        assert!(!req.includes(ReportSection::Plans));
    }

    #[test]
    fn request_with_period() {
        let req = ReportRequest::new(vec![ReportSection::Savings])
            .with_period(voom_domain::stats::TimePeriod::Month);
        assert_eq!(req.period, Some(voom_domain::stats::TimePeriod::Month));
    }

    #[test]
    fn request_with_history_limit() {
        let req = ReportRequest::new(vec![ReportSection::History]).with_history_limit(50);
        assert_eq!(req.history_limit, Some(50));
    }

    #[test]
    fn request_all_has_default_history_limit() {
        let req = ReportRequest::all();
        assert_eq!(req.history_limit, Some(20));
    }

    #[test]
    fn request_new_has_no_period_or_limit() {
        let req = ReportRequest::new(vec![ReportSection::Library]);
        assert!(req.period.is_none());
        assert!(req.history_limit.is_none());
    }
}
