//! Report query types and assembly logic.

use std::path::PathBuf;

use serde::Serialize;
use voom_domain::stats::{LibrarySnapshot, SavingsReport, TimePeriod};
use voom_domain::storage::{PageStats, PlanPhaseStat};
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
