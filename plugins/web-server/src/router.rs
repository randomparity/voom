//! Axum router construction.

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;

use crate::api;
use crate::middleware::{auth_middleware, SecurityHeadersLayer};
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
        .route("/policy/format", post(api::policy::format_policy));

    let page_routes = Router::new()
        .route("/", get(templates::dashboard))
        .route("/library", get(templates::library))
        .route("/files/:id", get(templates::file_detail))
        .route("/policies", get(templates::policies))
        .route("/policies/:name/edit", get(templates::policy_editor))
        .route("/jobs", get(templates::jobs_page))
        .route("/plugins", get(templates::plugins_page))
        .route("/settings", get(templates::settings));

    // Auth middleware protects API routes and the SSE endpoint.
    // Page routes (HTML) remain public.
    let authenticated_routes = Router::new()
        .nest("/api", api_routes)
        .route("/events", get(sse::events_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .merge(authenticated_routes)
        .merge(page_routes)
        .layer(SecurityHeadersLayer)
        .with_state(state)
}
