//! Report query types and assembly logic.

use std::path::PathBuf;

use serde::Serialize;
use voom_domain::stats::{LibrarySnapshot, SavingsReport, TimePeriod};
use voom_domain::storage::{FileFilters, PageStats, PlanPhaseStat, StorageTrait};
use voom_domain::SafeguardViolation;

/// Sections that can be included in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportSection {
    Library,
    Plans,
    Savings,
    History,
    Issues,
    Database,
}

/// Describes which sections and parameters to include in a report.
#[derive(Debug, Clone)]
pub struct ReportRequest {
    pub sections: Vec<ReportSection>,
    pub period: Option<TimePeriod>,
    pub history_limit: Option<u32>,
}

impl ReportRequest {
    #[must_use]
    pub fn new(sections: Vec<ReportSection>) -> Self {
        Self {
            sections,
            period: None,
            history_limit: None,
        }
    }

    /// Request all sections with a default history limit of 20.
    #[must_use]
    pub fn all() -> Self {
        Self {
            sections: vec![],
            period: None,
            history_limit: Some(20),
        }
    }

    #[must_use]
    pub fn summary() -> Self {
        Self::new(vec![ReportSection::Library])
    }

    #[must_use]
    pub fn with_period(mut self, period: TimePeriod) -> Self {
        self.period = Some(period);
        self
    }

    #[must_use]
    pub fn with_history_limit(mut self, limit: u32) -> Self {
        self.history_limit = Some(limit);
        self
    }

    /// Returns true if the given section should be included.
    ///
    /// An empty `sections` vec means "all sections".
    #[must_use]
    pub fn includes(&self, section: ReportSection) -> bool {
        self.sections.is_empty() || self.sections.contains(&section)
    }
}

/// Files with safeguard violations.
#[derive(Debug, Clone, Serialize)]
pub struct IssueReport {
    pub path: PathBuf,
    pub violations: Vec<SafeguardViolation>,
}

/// Database-level statistics (row counts and page stats).
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseStats {
    pub table_counts: Vec<(String, u64)>,
    pub page_stats: PageStats,
}

/// Assembled report result containing requested sections.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReportResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library: Option<LibrarySnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plans: Option<Vec<PlanPhaseStat>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub savings: Option<SavingsReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<LibrarySnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<IssueReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<DatabaseStats>,
}

/// Query the library and assemble the requested report sections.
///
/// # Errors
/// Returns a plugin error when an underlying storage query fails.
pub fn assemble_report(
    store: &dyn StorageTrait,
    request: &ReportRequest,
) -> voom_domain::Result<ReportResult> {
    let mut result = ReportResult::default();

    if request.includes(ReportSection::Library) {
        let snapshot = store
            .gather_library_stats(voom_domain::stats::SnapshotTrigger::Manual)
            .map_err(|e| crate::plugin_err("failed to gather library statistics", e))?;
        result.library = Some(snapshot);
    }

    if request.includes(ReportSection::Plans) {
        let stats = store
            .plan_stats_by_phase()
            .map_err(|e| crate::plugin_err("failed to query plan stats", e))?;
        result.plans = Some(stats);
    }

    if request.includes(ReportSection::Savings) {
        let report = store
            .savings_by_provenance(request.period)
            .map_err(|e| crate::plugin_err("failed to query savings", e))?;
        result.savings = Some(report);
    }

    if request.includes(ReportSection::History) {
        let limit = request.history_limit.unwrap_or(20);
        let snapshots = store
            .list_snapshots(limit)
            .map_err(|e| crate::plugin_err("failed to list snapshots", e))?;
        result.history = Some(snapshots);
    }

    if request.includes(ReportSection::Issues) {
        result.issues = Some(issue_report(store)?);
    }

    if request.includes(ReportSection::Database) {
        result.database = Some(database_stats(store)?);
    }

    Ok(result)
}

fn issue_report(store: &dyn StorageTrait) -> voom_domain::Result<Vec<IssueReport>> {
    let files = store
        .list_files(&FileFilters::default())
        .map_err(|e| crate::plugin_err("failed to list files", e))?;

    Ok(files
        .iter()
        .filter_map(|f| {
            let violations_val = f.plugin_metadata.get("safeguard_violations")?;
            let violations: Vec<SafeguardViolation> =
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
            Some(IssueReport {
                path: f.path.clone(),
                violations,
            })
        })
        .collect())
}

fn database_stats(store: &dyn StorageTrait) -> voom_domain::Result<DatabaseStats> {
    let table_counts = store
        .table_row_counts()
        .map_err(|e| crate::plugin_err("failed to query table row counts", e))?;
    let page_stats = store
        .page_stats()
        .map_err(|e| crate::plugin_err("failed to query page stats", e))?;
    Ok(DatabaseStats {
        table_counts,
        page_stats,
    })
}
