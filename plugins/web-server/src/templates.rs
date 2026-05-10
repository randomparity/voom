//! Tera template rendering and page handlers.

use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::response::Html;
use serde::Deserialize;
use std::collections::HashSet;

use voom_domain::job::JobStatus;
use voom_domain::storage::{FileFilters, JobFilters};
use voom_domain::verification::{VerificationFilters, VerificationOutcome};

use crate::api::files::FileFilterParams;
use crate::errors::{WebError, spawn_store_op};
use crate::middleware::CspNonce;
use crate::state::AppState;
use crate::views::{IntegrityErrorView, file_views, transition_views, verification_views};

type HtmlResult = Result<Html<String>, WebError>;
const FILE_VERIFICATION_LIMIT: u32 = 25;
const INTEGRITY_SCAN_LIMIT: u32 = 10_000;
const INTEGRITY_DISPLAY_LIMIT: usize = 500;

fn render(
    templates: &tera::Tera,
    name: &str,
    ctx: &mut tera::Context,
    nonce: &CspNonce,
) -> HtmlResult {
    ctx.insert("csp_nonce", &nonce.0);
    templates.render(name, ctx).map(Html).map_err(|e| {
        tracing::error!(template = name, error = %e, "Template render failed");
        WebError::Internal(format!("template render failed: {e}"))
    })
}

/// GET / -- Dashboard
pub async fn dashboard(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let store = state.store.clone();

    let (files, total_files, job_counts) = spawn_store_op(move || {
        let total_files = store.count_files(&FileFilters::default())?;
        let mut file_filters = FileFilters::default();
        file_filters.limit = Some(10);
        let files = store.list_files(&file_filters)?;
        let job_counts = store.count_jobs_by_status()?;
        Ok((files, total_files, job_counts))
    })
    .await?;

    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "dashboard");
    ctx.insert("recent_files", &file_views(files));
    ctx.insert("total_files", &total_files);
    for (status, count) in &job_counts {
        ctx.insert(format!("jobs_{}", status.as_str()), count);
    }

    render(&state.templates, "dashboard.html", &mut ctx, &nonce)
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct LibraryParams {
    #[serde(flatten)]
    pub filters: FileFilterParams,
    pub page: Option<u32>,
}

/// GET /library -- File browser
pub async fn library(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
    Query(params): Query<LibraryParams>,
) -> HtmlResult {
    let store = state.store.clone();
    let page = params.page.unwrap_or(1);
    let per_page = 50u32;
    let offset = (page - 1) * per_page;

    let mut filters = params.filters.to_file_filters();
    filters.limit = Some(per_page);
    filters.offset = Some(offset);

    let files = spawn_store_op(move || store.list_files(&filters)).await?;

    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "library");
    ctx.insert("files", &file_views(files));
    ctx.insert("page", &page);
    ctx.insert("per_page", &per_page);
    ctx.insert("filter_container", &params.filters.container);
    ctx.insert("filter_codec", &params.filters.codec);
    ctx.insert("filter_language", &params.filters.language);
    ctx.insert("filter_path_prefix", &params.filters.path_prefix);

    render(&state.templates, "library.html", &mut ctx, &nonce)
}

/// GET /files/:id -- File detail
pub async fn file_detail(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
    Path(id): Path<uuid::Uuid>,
) -> HtmlResult {
    let store = state.store.clone();

    let file_id = id.to_string();
    let (file, plans, transitions, verifications) = spawn_store_op(move || {
        let file = store.file(&id)?;
        let plans = store.plans_for_file(&id)?;
        let transitions = store.transitions_for_file(&id)?;
        let mut filters = VerificationFilters::default();
        filters.file_id = Some(file_id);
        filters.limit = Some(FILE_VERIFICATION_LIMIT);
        let verifications = store.list_verifications(&filters)?;
        Ok((file, plans, transitions, verifications))
    })
    .await?;

    let file = file.ok_or_else(|| WebError::NotFound(format!("File {id} not found")))?;

    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "library");
    let tracks_json: Vec<serde_json::Value> = file
        .tracks
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or_default())
        .collect();
    let file_view = crate::views::FileView::from_media_file(file);
    ctx.insert("file", &file_view);
    ctx.insert("tracks", &tracks_json);
    ctx.insert("plans", &plans);
    let transition_views_data = transition_views(transitions);
    ctx.insert("transitions", &transition_views_data);
    let verification_views_data = verification_views(verifications);
    ctx.insert("verifications", &verification_views_data);

    render(&state.templates, "file_detail.html", &mut ctx, &nonce)
}

/// GET /integrity -- Integrity failures
pub async fn integrity(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let store = state.store.clone();

    let failing_files = spawn_store_op(move || {
        let mut filters = VerificationFilters::default();
        filters.limit = Some(INTEGRITY_SCAN_LIMIT);
        let records = store.list_verifications(&filters)?;
        latest_error_files(&*store, records)
    })
    .await?;

    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "integrity");
    ctx.insert("failing_files", &failing_files);
    ctx.insert("display_limit", &INTEGRITY_DISPLAY_LIMIT);

    render(&state.templates, "integrity.html", &mut ctx, &nonce)
}

/// GET /estimates -- Pre-flight estimate records
pub async fn estimates(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "estimates");
    render(&state.templates, "estimates.html", &mut ctx, &nonce)
}

fn latest_error_files(
    store: &dyn voom_domain::storage::StorageTrait,
    records: Vec<voom_domain::verification::VerificationRecord>,
) -> voom_domain::errors::Result<Vec<IntegrityErrorView>> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();

    for record in records {
        if !seen.insert(record.file_id.clone()) {
            continue;
        }
        if record.outcome != VerificationOutcome::Error {
            continue;
        }
        if let Ok(file_id) = uuid::Uuid::parse_str(&record.file_id) {
            if let Some(file) = store.file(&file_id)? {
                rows.push(IntegrityErrorView::new(file, record));
            }
        }
        if rows.len() >= INTEGRITY_DISPLAY_LIMIT {
            break;
        }
    }

    Ok(rows)
}

/// GET /policies -- Policy list
pub async fn policies(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "policies");
    render(&state.templates, "policies.html", &mut ctx, &nonce)
}

/// GET /policies/:name/edit -- Policy editor
pub async fn policy_editor(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
    Path(name): Path<String>,
) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "policies");
    ctx.insert("policy_name", &name);
    render(&state.templates, "policy_editor.html", &mut ctx, &nonce)
}

#[derive(Debug, Deserialize)]
pub struct JobsPageParams {
    pub status: Option<String>,
}

/// GET /jobs -- Job monitor
pub async fn jobs(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
    Query(params): Query<JobsPageParams>,
) -> HtmlResult {
    let store = state.store.clone();
    let filter_status = params.status.as_deref().and_then(JobStatus::parse);

    let (jobs, counts) = spawn_store_op(move || {
        let mut job_filters = JobFilters::default();
        job_filters.status = filter_status;
        let jobs = store.list_jobs(&job_filters)?;
        let counts = store.count_jobs_by_status()?;
        Ok((jobs, counts))
    })
    .await?;

    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "jobs");
    ctx.insert("jobs", &jobs);
    ctx.insert("filter_status", &params.status.as_deref().unwrap_or(""));
    for (status, count) in &counts {
        ctx.insert(format!("jobs_{}", status.as_str()), count);
    }

    render(&state.templates, "jobs.html", &mut ctx, &nonce)
}

/// GET /plugins -- Plugin manager
pub async fn plugins(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "plugins");
    render(&state.templates, "plugins.html", &mut ctx, &nonce)
}

/// GET /settings -- Configuration
pub async fn settings(
    State(state): State<AppState>,
    Extension(nonce): Extension<CspNonce>,
) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("current_page", "settings");
    render(&state.templates, "settings.html", &mut ctx, &nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tera() -> tera::Tera {
        crate::server::embedded_templates().unwrap()
    }

    fn test_nonce() -> CspNonce {
        CspNonce("test-nonce-value".into())
    }

    #[test]
    fn test_render_success_returns_html() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let result = render(&tera, "policies.html", &mut ctx, &test_nonce());
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("html"), "Expected HTML content");
    }

    #[test]
    fn test_render_injects_csp_nonce() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let nonce = CspNonce("abc123".into());
        let result = render(&tera, "policies.html", &mut ctx, &nonce);
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(
            html.contains(r#"nonce="abc123""#),
            "Expected nonce attribute in rendered HTML"
        );
    }

    #[test]
    fn test_render_missing_template_returns_500() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let result = render(&tera, "nonexistent.html", &mut ctx, &test_nonce());
        assert!(result.is_err());
    }

    #[test]
    fn test_render_settings_page() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let result = render(&tera, "settings.html", &mut ctx, &test_nonce());
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_plugins_page() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let result = render(&tera, "plugins.html", &mut ctx, &test_nonce());
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_estimates_page() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        let result = render(&tera, "estimates.html", &mut ctx, &test_nonce());
        assert!(result.is_ok());
    }

    #[test]
    fn test_render_policy_editor_with_name() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("policy_name", "my-policy");
        let result = render(&tera, "policy_editor.html", &mut ctx, &test_nonce());
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("my-policy"));
    }

    #[test]
    fn test_library_params_defaults() {
        // Verify LibraryParams can deserialize with all optional fields absent
        let params: LibraryParams = serde_json::from_str("{}").unwrap();
        assert!(params.filters.container.is_none());
        assert!(params.filters.codec.is_none());
        assert!(params.filters.language.is_none());
        assert!(params.filters.path_prefix.is_none());
        assert!(params.page.is_none());
    }

    #[test]
    fn test_library_params_with_values() {
        let params: LibraryParams =
            serde_json::from_str(r#"{"container":"mkv","codec":"hevc","page":3}"#).unwrap();
        assert_eq!(params.filters.container, Some("mkv".to_string()));
        assert_eq!(params.filters.codec, Some("hevc".to_string()));
        assert_eq!(params.page, Some(3));
    }
}
