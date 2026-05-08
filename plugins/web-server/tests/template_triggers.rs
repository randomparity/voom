//! Static regression tests asserting that templates declare the htmx
//! triggers required to consume SSE-driven custom events.
//!
//! Issue #134: the dashboard must refresh when `voom:file-update` (and
//! `voom:job-update`) custom events are dispatched, so the live file count
//! advances as `FileIntrospected` events flow through the bridge.
//!
//! These tests intentionally grep the embedded template strings rather than
//! rendering them, because the triggers are static markup that must survive
//! template edits — they have no runtime branching to test.

const DASHBOARD_HTML: &str = include_str!("../templates/dashboard.html");
const FILE_DETAIL_HTML: &str = include_str!("../templates/file_detail.html");
const INTEGRITY_HTML: &str = include_str!("../templates/integrity.html");
const JOBS_HTML: &str = include_str!("../templates/jobs.html");
const BASE_HTML: &str = include_str!("../templates/base.html");

#[test]
fn dashboard_listens_for_file_update_events() {
    assert!(
        DASHBOARD_HTML.contains("voom:file-update from:body"),
        "dashboard.html must register an htmx trigger on `voom:file-update from:body` \
         so the total-files stat card refreshes when FileIntrospected events arrive \
         (issue #134)"
    );
}

#[test]
fn dashboard_listens_for_job_update_events() {
    assert!(
        DASHBOARD_HTML.contains("voom:job-update from:body"),
        "dashboard.html must register an htmx trigger on `voom:job-update from:body` \
         so the job counter cards refresh when job lifecycle events arrive (issue #134)"
    );
}

#[test]
fn dashboard_listens_for_plan_update_events() {
    assert!(
        DASHBOARD_HTML.contains("voom:plan-update from:body"),
        "dashboard.html must register an htmx trigger on `voom:plan-update from:body` \
         so job counters refresh when Plan* lifecycle events arrive via the \
         web-sse-bridge (issue #138)"
    );
}

#[test]
fn dashboard_contains_integrity_widget() {
    assert!(
        DASHBOARD_HTML.contains("Library Integrity")
            && DASHBOARD_HTML.contains("/api/integrity-summary")
            && DASHBOARD_HTML.contains("/integrity"),
        "dashboard.html must include a library integrity widget backed by \
         /api/integrity-summary and linked to the integrity page (issue #247)"
    );
}

#[test]
fn file_detail_contains_verification_history() {
    assert!(
        FILE_DETAIL_HTML.contains("Verification History"),
        "file_detail.html must render per-file verification history (issue #247)"
    );
}

#[test]
fn integrity_page_lists_failing_files() {
    assert!(
        INTEGRITY_HTML.contains("Latest Error Files")
            && INTEGRITY_HTML.contains("Last Verified")
            && INTEGRITY_HTML.contains("Hash Mismatch"),
        "integrity.html must list latest failing files with sortable verification \
         timing and distinct hash mismatch treatment (issue #247)"
    );
}

#[test]
fn jobs_page_listens_for_job_update_events() {
    assert!(
        JOBS_HTML.contains("voom:job-update from:body"),
        "jobs.html must register an htmx trigger on `voom:job-update from:body` \
         so the jobs table refreshes when job lifecycle events arrive (issue #134)"
    );
}

#[test]
fn base_dispatches_named_sse_events_as_custom_events() {
    // Sanity check the bridge between the EventSource listeners and the
    // htmx triggers above. If either half is renamed without updating the
    // other, the dashboard goes silent.
    for needle in [
        "addEventListener('job-update'",
        "addEventListener('file-update'",
        "voom:job-update",
        "voom:file-update",
    ] {
        assert!(
            BASE_HTML.contains(needle),
            "base.html missing required SSE wiring: {needle}"
        );
    }
}

#[test]
fn base_dispatches_voom_events_so_htmx_from_body_can_catch_them() {
    // htmx's `from:body` trigger modifier requires the event to bubble up to
    // <body>. CustomEvent defaults to bubbles:false, and dispatching on
    // `document` puts the event above <body> in the DOM hierarchy. The fix
    // for #134 is to dispatch on `document.body` with `bubbles: true`. This
    // test pins those two requirements so a future refactor cannot silently
    // break the live-update path again.
    assert!(
        BASE_HTML.contains("document.body.dispatchEvent"),
        "base.html must dispatch SSE-derived custom events on `document.body`, \
         not `document` — htmx `from:body` only fires on bubbled events that \
         reach the <body> element (issue #134)"
    );
    assert!(
        BASE_HTML.contains("bubbles: true"),
        "base.html must set `bubbles: true` on the SSE-derived CustomEvents so \
         they propagate to the <body> listener registered by htmx (issue #134)"
    );
    assert!(
        !BASE_HTML.contains("document.dispatchEvent(new CustomEvent('voom:"),
        "base.html still dispatches a `voom:*` event on `document` instead of \
         `document.body` — these will never reach htmx `from:body` triggers \
         (issue #134)"
    );
}
