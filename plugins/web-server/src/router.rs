//! Axum router construction.

use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower::limit::ConcurrencyLimitLayer;

use crate::api;
use crate::errors::ApiError;
use crate::middleware::{auth_middleware, RequestIdLayer, SecurityHeadersLayer};
use crate::sse;
use crate::state::AppState;
use crate::templates;

/// Build the complete application router.
pub fn build_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/files", get(api::files::list_files))
        .route(
            "/files/:id",
            get(api::files::get_file).delete(api::files::delete_file),
        )
        .route("/jobs", get(api::jobs::list_jobs))
        .route("/jobs/stats", get(api::jobs::job_stats))
        .route("/jobs/:id", get(api::jobs::get_job))
        .route("/plugins", get(api::plugins::list_plugins))
        .route("/stats", get(api::stats::get_stats))
        .route("/policy/validate", post(api::policy::validate_policy))
        .route("/policy/format", post(api::policy::format_policy))
        .route("/tools", get(api::tools::list_tools));

    let page_routes = Router::new()
        .route("/", get(templates::dashboard))
        .route("/library", get(templates::library))
        .route("/files/:id", get(templates::file_detail))
        .route("/policies", get(templates::policies))
        .route("/policies/:name/edit", get(templates::policy_editor))
        .route("/jobs", get(templates::jobs_page))
        .route("/plugins", get(templates::plugins_page))
        .route("/settings", get(templates::settings));

    // Auth middleware protects all routes (API, SSE, and HTML pages) when
    // an auth_token is configured. Without a token, all routes are public.
    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/events", get(sse::events_handler))
        .merge(page_routes)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(ConcurrencyLimitLayer::new(100));

    Router::new()
        .merge(authenticated_routes)
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError {
                    error: "Not found".into(),
                    details: None,
                }),
            )
        })
        .layer(RequestIdLayer)
        .layer(SecurityHeadersLayer)
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use voom_domain::test_support::InMemoryStore;

    fn make_state(auth_token: Option<String>) -> AppState {
        let store = Arc::new(InMemoryStore::new());
        let templates = tera::Tera::default();
        AppState::new(store, templates, auth_token)
    }

    #[test]
    fn build_router_returns_valid_router() {
        let state = make_state(None);
        // Should not panic — validates that all routes wire up correctly
        let _router = build_router(state);
    }

    #[test]
    fn build_router_with_auth_token() {
        let state = make_state(Some("test-token".into()));
        let _router = build_router(state);
    }
}
