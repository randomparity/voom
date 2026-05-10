//! Axum router construction.

use axum::http::{header, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower::limit::ConcurrencyLimitLayer;

use crate::api;
use crate::errors::ApiError;
use crate::middleware::{auth_middleware, RateLimitLayer, RequestIdLayer, SecurityHeadersLayer};
use crate::sse;
use crate::state::AppState;
use crate::templates;

static HTMX_JS: &[u8] = include_bytes!("../static/htmx-2.0.4.min.js");
static ALPINE_JS: &[u8] = include_bytes!("../static/alpine-3.14.8.min.js");

/// Build the complete application router.
pub fn build_router(state: AppState) -> Router {
    let api_routes = Router::new()
        .route("/files", get(api::files::list_files))
        .route("/estimates", get(api::estimates::list_estimates))
        .route("/estimates/:id", get(api::estimates::get_estimate))
        .route(
            "/files/:id",
            get(api::files::get_file).delete(api::files::delete_file),
        )
        .route(
            "/files/:id/transitions",
            get(api::transitions::list_transitions),
        )
        .route("/jobs", get(api::jobs::list_jobs))
        .route("/jobs/stats", get(api::jobs::get_job_stats))
        .route("/jobs/:id", get(api::jobs::get_job))
        .route("/plugins", get(api::plugins::list_plugins))
        .route("/stats/loudness", get(api::stats::get_loudness_stats))
        .route("/stats/library", get(api::stats::get_library_stats))
        .route("/stats/history", get(api::stats::get_stats_history))
        .route("/stats", get(api::stats::get_stats))
        .route("/policy/validate", post(api::policy::validate_policy))
        .route("/policy/format", post(api::policy::format_policy))
        .route("/tools", get(api::tools::list_tools))
        .route(
            "/executor-capabilities",
            get(api::tools::list_executor_capabilities),
        )
        .route("/health", get(api::health::get_health))
        .route("/verify", get(api::verify::list_verifications))
        .route("/verify/:file_id", get(api::verify::get_file_verifications))
        .route("/integrity-summary", get(api::verify::integrity_summary));

    let page_routes = Router::new()
        .route("/", get(templates::dashboard))
        .route("/library", get(templates::library))
        .route("/files/:id", get(templates::file_detail))
        .route("/integrity", get(templates::integrity))
        .route("/estimates", get(templates::estimates))
        .route("/policies", get(templates::policies))
        .route("/policies/:name/edit", get(templates::policy_editor))
        .route("/jobs", get(templates::jobs))
        .route("/plugins", get(templates::plugins))
        .route("/settings", get(templates::settings));

    let static_routes = Router::new()
        .route("/static/htmx-2.0.4.min.js", get(static_htmx))
        .route("/static/alpine-3.14.8.min.js", get(static_alpine));

    // Auth middleware protects all routes (API, SSE, HTML pages, and the
    // 404 fallback) when an auth_token is configured. Without a token, all
    // routes are public. The fallback lives inside this router so unknown
    // paths go through the auth layer and return 401 (not 404) when
    // unauthenticated.
    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/events", get(sse::events_handler))
        .merge(page_routes)
        .merge(static_routes)
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                axum::Json(ApiError {
                    error: "Not found".into(),
                    details: None,
                }),
            )
        })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(ConcurrencyLimitLayer::new(100))
        .layer(RateLimitLayer::new());

    Router::new()
        .merge(authenticated_routes)
        .layer(RequestIdLayer)
        .layer(SecurityHeadersLayer)
        .with_state(state)
}

async fn static_htmx() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        HTMX_JS,
    )
}

async fn static_alpine() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "application/javascript"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        ALPINE_JS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::make_test_state;

    fn make_state(auth_token: Option<String>) -> AppState {
        make_test_state(auth_token)
    }

    #[test]
    fn test_build_router_returns_valid_router() {
        let state = make_state(None);
        // Should not panic — validates that all routes wire up correctly
        let _router = build_router(state);
    }

    #[test]
    fn test_build_router_with_auth_token() {
        let state = make_state(Some("test-token".into()));
        let _router = build_router(state);
    }
}
