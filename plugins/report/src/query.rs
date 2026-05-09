//! Report query types and assembly logic.

use std::path::PathBuf;

use serde::Serialize;
use voom_domain::stats::{LibrarySnapshot, SavingsReport, TimePeriod};
use voom_domain::storage::{
    FileFilters, FileStorage, FileTransitionStorage, MaintenanceStorage, PageStats, PlanPhaseStat,
    PlanStorage, SnapshotStorage, StorageTrait,
};
use voom_domain::{MediaFile, SafeguardViolation};

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

trait ReportDataSource {
    fn gather_library_stats(
        &self,
        trigger: voom_domain::stats::SnapshotTrigger,
    ) -> voom_domain::Result<LibrarySnapshot>;
    fn plan_stats_by_phase(&self) -> voom_domain::Result<Vec<PlanPhaseStat>>;
    fn savings_by_provenance(
        &self,
        period: Option<TimePeriod>,
    ) -> voom_domain::Result<SavingsReport>;
    fn list_snapshots(&self, limit: u32) -> voom_domain::Result<Vec<LibrarySnapshot>>;
    fn list_files(&self, filters: &FileFilters) -> voom_domain::Result<Vec<MediaFile>>;
    fn table_row_counts(&self) -> voom_domain::Result<Vec<(String, u64)>>;
    fn page_stats(&self) -> voom_domain::Result<PageStats>;
}

impl<T> ReportDataSource for T
where
    T: StorageTrait + ?Sized,
{
    fn gather_library_stats(
        &self,
        trigger: voom_domain::stats::SnapshotTrigger,
    ) -> voom_domain::Result<LibrarySnapshot> {
        SnapshotStorage::gather_library_stats(self, trigger)
    }

    fn plan_stats_by_phase(&self) -> voom_domain::Result<Vec<PlanPhaseStat>> {
        PlanStorage::plan_stats_by_phase(self)
    }

    fn savings_by_provenance(
        &self,
        period: Option<TimePeriod>,
    ) -> voom_domain::Result<SavingsReport> {
        FileTransitionStorage::savings_by_provenance(self, period)
    }

    fn list_snapshots(&self, limit: u32) -> voom_domain::Result<Vec<LibrarySnapshot>> {
        SnapshotStorage::list_snapshots(self, limit)
    }

    fn list_files(&self, filters: &FileFilters) -> voom_domain::Result<Vec<MediaFile>> {
        FileStorage::list_files(self, filters)
    }

    fn table_row_counts(&self) -> voom_domain::Result<Vec<(String, u64)>> {
        MaintenanceStorage::table_row_counts(self)
    }

    fn page_stats(&self) -> voom_domain::Result<PageStats> {
        MaintenanceStorage::page_stats(self)
    }
}

/// Query the library and assemble the requested report sections.
///
/// # Errors
/// Returns a plugin error when an underlying storage query fails.
pub fn assemble_report(
    store: &dyn StorageTrait,
    request: &ReportRequest,
) -> voom_domain::Result<ReportResult> {
    assemble_report_from_source(store, request)
}

fn assemble_report_from_source(
    store: &(impl ReportDataSource + ?Sized),
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

fn issue_report(store: &(impl ReportDataSource + ?Sized)) -> voom_domain::Result<Vec<IssueReport>> {
    let files = store
        .list_files(&FileFilters::default())
        .map_err(|e| crate::plugin_err("failed to list files", e))?;

    let mut issues = Vec::new();
    for file in files {
        let Some(violations_val) = file.plugin_metadata.get("safeguard_violations") else {
            continue;
        };
        let violations: Vec<SafeguardViolation> = serde_json::from_value(violations_val.clone())
            .map_err(|e| {
                crate::plugin_err(
                    &format!(
                        "malformed safeguard_violations metadata for {}",
                        file.path.display()
                    ),
                    e,
                )
            })?;
        if violations.is_empty() {
            continue;
        }
        issues.push(IssueReport {
            path: file.path,
            violations,
        });
    }
    Ok(issues)
}

fn database_stats(store: &(impl ReportDataSource + ?Sized)) -> voom_domain::Result<DatabaseStats> {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use voom_domain::errors::VoomError;
    use voom_domain::safeguard::{SafeguardKind, SafeguardViolation};
    use voom_domain::stats::{LibrarySnapshot, SavingsReport, SnapshotTrigger};
    use voom_domain::storage::{PageStats, PlanPhaseStat, PlanStatus};

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Library,
        Plans,
        Savings,
        History,
        Issues,
        TableCounts,
        PageStats,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FailPoint {
        Library,
        Plans,
        Savings,
        History,
        Issues,
        TableCounts,
        PageStats,
    }

    #[derive(Default)]
    struct FakeReportStore {
        calls: Mutex<Vec<Call>>,
        history_limits: Mutex<Vec<u32>>,
        files: Vec<MediaFile>,
        fail: Option<FailPoint>,
    }

    impl FakeReportStore {
        fn with_files(files: Vec<MediaFile>) -> Self {
            Self {
                files,
                ..Self::default()
            }
        }

        fn failing(fail: FailPoint) -> Self {
            Self {
                fail: Some(fail),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("calls lock").clone()
        }

        fn history_limits(&self) -> Vec<u32> {
            self.history_limits
                .lock()
                .expect("history limits lock")
                .clone()
        }

        fn record(&self, call: Call) {
            self.calls.lock().expect("calls lock").push(call);
        }

        fn fail_if(&self, fail_point: FailPoint) -> voom_domain::Result<()> {
            if self.fail == Some(fail_point) {
                return Err(VoomError::Storage {
                    kind: voom_domain::errors::StorageErrorKind::Other,
                    message: format!("{fail_point:?} failed"),
                });
            }
            Ok(())
        }
    }

    impl ReportDataSource for FakeReportStore {
        fn gather_library_stats(
            &self,
            trigger: SnapshotTrigger,
        ) -> voom_domain::Result<LibrarySnapshot> {
            self.record(Call::Library);
            self.fail_if(FailPoint::Library)?;
            Ok(snapshot(trigger))
        }

        fn plan_stats_by_phase(&self) -> voom_domain::Result<Vec<PlanPhaseStat>> {
            self.record(Call::Plans);
            self.fail_if(FailPoint::Plans)?;
            Ok(vec![PlanPhaseStat::new(
                "phase".to_string(),
                PlanStatus::Completed,
                None,
                1,
            )])
        }

        fn savings_by_provenance(
            &self,
            period: Option<TimePeriod>,
        ) -> voom_domain::Result<SavingsReport> {
            self.record(Call::Savings);
            self.fail_if(FailPoint::Savings)?;
            assert!(period.is_none() || period == Some(TimePeriod::Month));
            Ok(SavingsReport::default())
        }

        fn list_snapshots(&self, limit: u32) -> voom_domain::Result<Vec<LibrarySnapshot>> {
            self.record(Call::History);
            self.fail_if(FailPoint::History)?;
            self.history_limits
                .lock()
                .expect("history limits lock")
                .push(limit);
            Ok(vec![snapshot(SnapshotTrigger::Manual)])
        }

        fn list_files(&self, _filters: &FileFilters) -> voom_domain::Result<Vec<MediaFile>> {
            self.record(Call::Issues);
            self.fail_if(FailPoint::Issues)?;
            Ok(self.files.clone())
        }

        fn table_row_counts(&self) -> voom_domain::Result<Vec<(String, u64)>> {
            self.record(Call::TableCounts);
            self.fail_if(FailPoint::TableCounts)?;
            Ok(vec![("files".to_string(), 1)])
        }

        fn page_stats(&self) -> voom_domain::Result<PageStats> {
            self.record(Call::PageStats);
            self.fail_if(FailPoint::PageStats)?;
            Ok(PageStats {
                page_size: 4096,
                page_count: 1,
                freelist_count: 0,
            })
        }
    }

    fn snapshot(trigger: SnapshotTrigger) -> LibrarySnapshot {
        voom_domain::storage::SnapshotStorage::gather_library_stats(
            &voom_domain::test_support::InMemoryStore::new(),
            trigger,
        )
        .expect("in-memory snapshot")
    }

    fn media_file(path: &str) -> MediaFile {
        MediaFile::new(PathBuf::from(path))
    }

    fn violation() -> SafeguardViolation {
        SafeguardViolation::new(SafeguardKind::NoAudioTrack, "no audio", "normalize")
    }

    #[test]
    fn assemble_report_queries_only_requested_section() {
        let cases = [
            (ReportSection::Library, vec![Call::Library]),
            (ReportSection::Plans, vec![Call::Plans]),
            (ReportSection::Savings, vec![Call::Savings]),
            (ReportSection::History, vec![Call::History]),
            (ReportSection::Issues, vec![Call::Issues]),
            (
                ReportSection::Database,
                vec![Call::TableCounts, Call::PageStats],
            ),
        ];

        for (section, expected_calls) in cases {
            let store = FakeReportStore::default();
            assemble_report_from_source(&store, &ReportRequest::new(vec![section]))
                .expect("report should assemble");
            assert_eq!(store.calls(), expected_calls, "section={section:?}");
        }
    }

    #[test]
    fn assemble_report_uses_default_and_explicit_history_limits() {
        let store = FakeReportStore::default();
        assemble_report_from_source(&store, &ReportRequest::all()).expect("report should assemble");
        assert_eq!(store.history_limits(), vec![20]);

        let store = FakeReportStore::default();
        assemble_report_from_source(
            &store,
            &ReportRequest::new(vec![ReportSection::History]).with_history_limit(7),
        )
        .expect("report should assemble");
        assert_eq!(store.history_limits(), vec![7]);
    }

    #[test]
    fn assemble_report_wraps_storage_errors_with_section_context() {
        let cases = [
            (
                FailPoint::Library,
                ReportRequest::new(vec![ReportSection::Library]),
                "failed to gather library statistics",
            ),
            (
                FailPoint::Plans,
                ReportRequest::new(vec![ReportSection::Plans]),
                "failed to query plan stats",
            ),
            (
                FailPoint::Savings,
                ReportRequest::new(vec![ReportSection::Savings]),
                "failed to query savings",
            ),
            (
                FailPoint::History,
                ReportRequest::new(vec![ReportSection::History]),
                "failed to list snapshots",
            ),
            (
                FailPoint::Issues,
                ReportRequest::new(vec![ReportSection::Issues]),
                "failed to list files",
            ),
            (
                FailPoint::TableCounts,
                ReportRequest::new(vec![ReportSection::Database]),
                "failed to query table row counts",
            ),
            (
                FailPoint::PageStats,
                ReportRequest::new(vec![ReportSection::Database]),
                "failed to query page stats",
            ),
        ];

        for (fail_point, request, context) in cases {
            let err = assemble_report_from_source(&FakeReportStore::failing(fail_point), &request)
                .expect_err("report assembly should fail");
            let message = err.to_string();
            assert!(message.contains("plugin error: report:"));
            assert!(message.contains(context), "{message}");
            assert!(
                message.contains(&format!("{fail_point:?} failed")),
                "{message}"
            );
        }
    }

    #[test]
    fn issue_report_returns_valid_nonempty_safeguard_violations() {
        let mut file = media_file("/media/valid.mkv");
        file.plugin_metadata.insert(
            "safeguard_violations".to_string(),
            serde_json::to_value(vec![violation()]).expect("violation serializes"),
        );
        let store = FakeReportStore::with_files(vec![file]);

        let issues = issue_report(&store).expect("issue report should parse");

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, PathBuf::from("/media/valid.mkv"));
        assert_eq!(issues[0].violations, vec![violation()]);
    }

    #[test]
    fn issue_report_omits_missing_and_empty_safeguard_metadata() {
        let missing = media_file("/media/missing.mkv");
        let mut empty = media_file("/media/empty.mkv");
        empty.plugin_metadata.insert(
            "safeguard_violations".to_string(),
            serde_json::to_value(Vec::<SafeguardViolation>::new()).expect("empty serializes"),
        );
        let store = FakeReportStore::with_files(vec![missing, empty]);

        let issues = issue_report(&store).expect("issue report should parse");

        assert!(issues.is_empty());
    }

    #[test]
    fn issue_report_fails_on_malformed_safeguard_metadata() {
        let mut file = media_file("/media/bad.mkv");
        file.plugin_metadata.insert(
            "safeguard_violations".to_string(),
            serde_json::json!({"not": "a violation list"}),
        );
        let store = FakeReportStore::with_files(vec![file]);

        let err = issue_report(&store).expect_err("malformed metadata should fail");
        let message = err.to_string();
        assert!(message.contains("malformed safeguard_violations metadata"));
        assert!(message.contains("/media/bad.mkv"));
    }
}
