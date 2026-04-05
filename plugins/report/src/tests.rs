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
