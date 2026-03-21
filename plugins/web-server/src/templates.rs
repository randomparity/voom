//! Tera template rendering and page handlers.

use axum::extract::{Path, Query, State};
use axum::response::Html;
use serde::Deserialize;

use voom_domain::job::JobStatus;
use voom_domain::media::Container;
use voom_domain::storage::{FileFilters, JobFilters};

use crate::api::files::FileFilterParams;
use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;
use crate::views::file_views;

type HtmlResult = Result<Html<String>, WebError>;

fn render(templates: &tera::Tera, name: &str, ctx: &tera::Context) -> HtmlResult {
    templates.render(name, ctx).map(Html).map_err(|e| {
        tracing::error!(template = name, error = %e, "Template render failed");
        WebError::Internal(format!("template render failed: {e}"))
    })
}

/// GET / -- Dashboard
pub async fn dashboard(State(state): State<AppState>) -> HtmlResult {
    let store = state.store.clone();

    let (files, total_files, job_counts) = spawn_store_op(move || {
        let total_files = store.count_files(&FileFilters::default())?;
        let files = store.list_files(&FileFilters {
            limit: Some(10),
            ..Default::default()
        })?;
        let job_counts = store.count_jobs_by_status()?;
        Ok((files, total_files, job_counts))
    })
    .await?;

    let mut ctx = tera::Context::new();
    ctx.insert("recent_files", &file_views(files));
    ctx.insert("total_files", &total_files);
    for (status, count) in &job_counts {
        ctx.insert(format!("jobs_{}", status.as_str()), count);
    }

    render(&state.templates, "dashboard.html", &ctx)
}

#[derive(Debug, Deserialize)]
pub struct LibraryParams {
    #[serde(flatten)]
    pub filters: FileFilterParams,
    pub page: Option<u32>,
}

/// GET /library -- File browser
pub async fn library(
    State(state): State<AppState>,
    Query(params): Query<LibraryParams>,
) -> HtmlResult {
    let store = state.store.clone();
    let page = params.page.unwrap_or(1);
    let per_page = 50u32;
    let offset = (page - 1) * per_page;

    let filters = FileFilters {
        container: params
            .filters
            .container
            .as_deref()
            .map(Container::from_extension),
        has_codec: params.filters.codec.clone(),
        has_language: params.filters.language.clone(),
        path_prefix: params.filters.path_prefix.clone(),
        limit: Some(per_page),
        offset: Some(offset),
    };

    let files = spawn_store_op(move || store.list_files(&filters)).await?;

    let mut ctx = tera::Context::new();
    ctx.insert("files", &file_views(files));
    ctx.insert("page", &page);
    ctx.insert("per_page", &per_page);
    ctx.insert("filter_container", &params.filters.container);
    ctx.insert("filter_codec", &params.filters.codec);
    ctx.insert("filter_language", &params.filters.language);
    ctx.insert("filter_path_prefix", &params.filters.path_prefix);

    render(&state.templates, "library.html", &ctx)
}

/// GET /files/:id -- File detail
pub async fn file_detail(State(state): State<AppState>, Path(id): Path<uuid::Uuid>) -> HtmlResult {
    let store = state.store.clone();

    let (file, plans) = spawn_store_op(move || {
        let file = store.get_file(&id)?;
        let plans = store.get_plans_for_file(&id)?;
        Ok((file, plans))
    })
    .await?;

    let file = file.ok_or_else(|| WebError::NotFound(format!("File {id} not found")))?;

    // StoredPlan doesn't derive Serialize, so convert to JSON values manually
    let plans_json: Vec<serde_json::Value> = plans
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p.id.to_string(),
                "file_id": p.file_id.to_string(),
                "policy_name": p.policy_name,
                "phase_name": p.phase_name,
                "status": p.status,
                "actions_json": p.actions_json,
                "warnings": p.warnings,
                "created_at": p.created_at,
                "executed_at": p.executed_at,
                "result": p.result,
            })
        })
        .collect();

    let mut ctx = tera::Context::new();
    let tracks_json: Vec<serde_json::Value> = file
        .tracks
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or_default())
        .collect();
    let file_view = crate::views::FileView::from_media_file(file);
    ctx.insert("file", &file_view);
    ctx.insert("tracks", &tracks_json);
    ctx.insert("plans", &plans_json);

    render(&state.templates, "file_detail.html", &ctx)
}

/// GET /policies -- Policy list
pub async fn policies(State(state): State<AppState>) -> HtmlResult {
    let ctx = tera::Context::new();
    render(&state.templates, "policies.html", &ctx)
}

/// GET /policies/:name/edit -- Policy editor
pub async fn policy_editor(State(state): State<AppState>, Path(name): Path<String>) -> HtmlResult {
    let mut ctx = tera::Context::new();
    ctx.insert("policy_name", &name);
    render(&state.templates, "policy_editor.html", &ctx)
}

#[derive(Debug, Deserialize)]
pub struct JobsPageParams {
    pub status: Option<String>,
}

/// GET /jobs -- Job monitor
pub async fn jobs_page(
    State(state): State<AppState>,
    Query(params): Query<JobsPageParams>,
) -> HtmlResult {
    let store = state.store.clone();
    let filter_status = params.status.as_deref().and_then(JobStatus::parse);

    let (jobs, counts) = spawn_store_op(move || {
        let jobs = store.list_jobs(&JobFilters {
            status: filter_status,
            limit: None,
        })?;
        let counts = store.count_jobs_by_status()?;
        Ok((jobs, counts))
    })
    .await?;

    let mut ctx = tera::Context::new();
    ctx.insert("jobs", &jobs);
    ctx.insert("filter_status", &params.status.as_deref().unwrap_or(""));
    for (status, count) in &counts {
        ctx.insert(format!("jobs_{}", status.as_str()), count);
    }

    render(&state.templates, "jobs.html", &ctx)
}

/// GET /plugins -- Plugin manager
pub async fn plugins_page(State(state): State<AppState>) -> HtmlResult {
    let ctx = tera::Context::new();
    render(&state.templates, "plugins.html", &ctx)
}

/// GET /settings -- Configuration
pub async fn settings(State(state): State<AppState>) -> HtmlResult {
    let ctx = tera::Context::new();
    render(&state.templates, "settings.html", &ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tera() -> tera::Tera {
        crate::server::embedded_templates()
    }

    #[test]
    fn render_success_returns_html() {
        let tera = make_tera();
        let ctx = tera::Context::new();
        let result = render(&tera, "policies.html", &ctx);
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("html"), "Expected HTML content");
    }

    #[test]
    fn render_missing_template_returns_500() {
        let tera = make_tera();
        let ctx = tera::Context::new();
        let result = render(&tera, "nonexistent.html", &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn render_settings_page() {
        let tera = make_tera();
        let ctx = tera::Context::new();
        let result = render(&tera, "settings.html", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn render_plugins_page() {
        let tera = make_tera();
        let ctx = tera::Context::new();
        let result = render(&tera, "plugins.html", &ctx);
        assert!(result.is_ok());
    }

    #[test]
    fn render_policy_editor_with_name() {
        let tera = make_tera();
        let mut ctx = tera::Context::new();
        ctx.insert("policy_name", "my-policy");
        let result = render(&tera, "policy_editor.html", &ctx);
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("my-policy"));
    }

    #[test]
    fn library_params_defaults() {
        // Verify LibraryParams can deserialize with all optional fields absent
        let params: LibraryParams = serde_json::from_str("{}").unwrap();
        assert!(params.filters.container.is_none());
        assert!(params.filters.codec.is_none());
        assert!(params.filters.language.is_none());
        assert!(params.filters.path_prefix.is_none());
        assert!(params.page.is_none());
    }

    #[test]
    fn library_params_with_values() {
        let params: LibraryParams =
            serde_json::from_str(r#"{"container":"mkv","codec":"hevc","page":3}"#).unwrap();
        assert_eq!(params.filters.container, Some("mkv".to_string()));
        assert_eq!(params.filters.codec, Some("hevc".to_string()));
        assert_eq!(params.page, Some(3));
    }
}
