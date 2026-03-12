//! Tera template rendering and page handlers.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use serde::Deserialize;

use voom_domain::storage::FileFilters;

use crate::state::AppState;

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

    let files = tokio::task::spawn_blocking(move || {
        store.list_files(&FileFilters {
            limit: Some(10),
            ..Default::default()
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let job_counts = tokio::task::spawn_blocking(move || store2.count_jobs_by_status())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut ctx = tera::Context::new();
    ctx.insert("files", &files);
    ctx.insert(
        "job_counts",
        &serde_json::to_value(
            job_counts
                .iter()
                .map(|(s, c)| serde_json::json!({"status": format!("{:?}", s), "count": c}))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default(),
    );
    ctx.insert("total_files", &files.len());

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
    ctx.insert("files", &files);
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
    ctx.insert("file", &file);
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

/// GET /jobs -- Job monitor
pub async fn jobs_page(State(state): State<AppState>) -> HtmlResult {
    let store = state.store.clone();

    let jobs = tokio::task::spawn_blocking(move || store.list_jobs(None, Some(100)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut ctx = tera::Context::new();
    ctx.insert("jobs", &jobs);

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
