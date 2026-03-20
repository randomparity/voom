//! Tera template rendering and page handlers.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use serde::Deserialize;

use voom_domain::job::JobStatus;
use voom_domain::storage::{FileFilters, JobFilters};

use crate::state::AppState;
use crate::views::file_views;

type HtmlResult = Result<Html<String>, (StatusCode, String)>;

fn render(templates: &tera::Tera, name: &str, ctx: &tera::Context) -> HtmlResult {
    templates.render(name, ctx).map(Html).map_err(|e| {
        tracing::error!(template = name, error = %e, "Template render failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error".to_string(),
        )
    })
}

/// GET / -- Dashboard
pub async fn dashboard(State(state): State<AppState>) -> HtmlResult {
    let store = state.store.clone();
    let store2 = state.store.clone();
    let store3 = state.store.clone();

    let total_files_fut =
        tokio::task::spawn_blocking(move || store3.count_files(&FileFilters::default()));

    let files_fut = tokio::task::spawn_blocking(move || {
        store.list_files(&FileFilters {
            limit: Some(10),
            ..Default::default()
        })
    });

    let job_counts_fut = tokio::task::spawn_blocking(move || store2.count_jobs_by_status());

    let (total_files_res, files_res, job_counts_res) =
        tokio::try_join!(total_files_fut, files_fut, job_counts_fut)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let total_files =
        total_files_res.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let files = files_res.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let job_counts =
        job_counts_res.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
    pub container: Option<String>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub path_prefix: Option<String>,
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
        container: params.container.clone(),
        has_codec: params.codec.clone(),
        has_language: params.language.clone(),
        path_prefix: params.path_prefix.clone(),
        limit: Some(per_page),
        offset: Some(offset),
    };

    let files = tokio::task::spawn_blocking(move || store.list_files(&filters))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut ctx = tera::Context::new();
    ctx.insert("files", &file_views(files));
    ctx.insert("page", &page);
    ctx.insert("per_page", &per_page);
    ctx.insert("filter_container", &params.container);
    ctx.insert("filter_codec", &params.codec);
    ctx.insert("filter_language", &params.language);
    ctx.insert("filter_path_prefix", &params.path_prefix);

    render(&state.templates, "library.html", &ctx)
}

/// GET /files/:id -- File detail
pub async fn file_detail(State(state): State<AppState>, Path(id): Path<uuid::Uuid>) -> HtmlResult {
    let store = state.store.clone();
    let store2 = state.store.clone();

    let file = tokio::task::spawn_blocking(move || store.get_file(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let file = file.ok_or_else(|| (StatusCode::NOT_FOUND, format!("File {id} not found")))?;

    let plans = tokio::task::spawn_blocking(move || store2.get_plans_for_file(&id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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

    let (jobs, counts) = tokio::task::spawn_blocking(move || {
        let jobs = store.list_jobs(&JobFilters {
            status: filter_status,
            limit: None,
        })?;
        let counts = store.count_jobs_by_status()?;
        Ok::<_, voom_domain::errors::VoomError>((jobs, counts))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

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
        let (status, _msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
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
        assert!(params.container.is_none());
        assert!(params.codec.is_none());
        assert!(params.language.is_none());
        assert!(params.path_prefix.is_none());
        assert!(params.page.is_none());
    }

    #[test]
    fn library_params_with_values() {
        let params: LibraryParams =
            serde_json::from_str(r#"{"container":"mkv","codec":"hevc","page":3}"#).unwrap();
        assert_eq!(params.container, Some("mkv".to_string()));
        assert_eq!(params.codec, Some("hevc".to_string()));
        assert_eq!(params.page, Some(3));
    }
}
