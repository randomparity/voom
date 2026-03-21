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
    use std::path::Path;
    use std::sync::Arc;
    use voom_domain::errors::Result as VoomResult;
    use voom_domain::job::{Job, JobStatus, JobUpdate};
    use voom_domain::media::MediaFile;
    use voom_domain::plan::Plan;
    use voom_domain::stats::ProcessingStats;
    use voom_domain::storage::{FileFilters, JobFilters, StorageTrait, StoredPlan};

    struct DummyStore;
    impl StorageTrait for DummyStore {
        fn upsert_file(&self, _: &MediaFile) -> VoomResult<()> {
            Ok(())
        }
        fn get_file(&self, _: &uuid::Uuid) -> VoomResult<Option<MediaFile>> {
            Ok(None)
        }
        fn get_file_by_path(&self, _: &Path) -> VoomResult<Option<MediaFile>> {
            Ok(None)
        }
        fn list_files(&self, _: &FileFilters) -> VoomResult<Vec<MediaFile>> {
            Ok(vec![])
        }
        fn count_files(&self, _: &FileFilters) -> VoomResult<u64> {
            Ok(0)
        }
        fn delete_file(&self, _: &uuid::Uuid) -> VoomResult<()> {
            Ok(())
        }
        fn create_job(&self, _: &Job) -> VoomResult<uuid::Uuid> {
            Ok(uuid::Uuid::new_v4())
        }
        fn get_job(&self, _: &uuid::Uuid) -> VoomResult<Option<Job>> {
            Ok(None)
        }
        fn update_job(&self, _: &uuid::Uuid, _: &JobUpdate) -> VoomResult<()> {
            Ok(())
        }
        fn claim_next_job(&self, _: &str) -> VoomResult<Option<Job>> {
            Ok(None)
        }
        fn claim_job_by_id(&self, _: &uuid::Uuid, _: &str) -> VoomResult<Option<Job>> {
            Ok(None)
        }
        fn list_jobs(&self, _: &JobFilters) -> VoomResult<Vec<Job>> {
            Ok(vec![])
        }
        fn count_jobs_by_status(&self) -> VoomResult<Vec<(JobStatus, u64)>> {
            Ok(vec![])
        }
        fn save_plan(&self, _: &Plan) -> VoomResult<uuid::Uuid> {
            Ok(uuid::Uuid::new_v4())
        }
        fn get_plans_for_file(&self, _: &uuid::Uuid) -> VoomResult<Vec<StoredPlan>> {
            Ok(vec![])
        }
        fn update_plan_status(
            &self,
            _: &uuid::Uuid,
            _: voom_domain::storage::PlanStatus,
        ) -> VoomResult<()> {
            Ok(())
        }
        fn get_file_history(
            &self,
            _: &Path,
        ) -> VoomResult<Vec<voom_domain::storage::FileHistoryEntry>> {
            Ok(vec![])
        }
        fn record_stats(&self, _: &ProcessingStats) -> VoomResult<()> {
            Ok(())
        }
        fn get_plugin_data(&self, _: &str, _: &str) -> VoomResult<Option<Vec<u8>>> {
            Ok(None)
        }
        fn set_plugin_data(&self, _: &str, _: &str, _: &[u8]) -> VoomResult<()> {
            Ok(())
        }
        fn upsert_bad_file(&self, _: &voom_domain::bad_file::BadFile) -> VoomResult<()> {
            Ok(())
        }
        fn get_bad_file_by_path(
            &self,
            _: &Path,
        ) -> VoomResult<Option<voom_domain::bad_file::BadFile>> {
            Ok(None)
        }
        fn list_bad_files(
            &self,
            _: &voom_domain::storage::BadFileFilters,
        ) -> VoomResult<Vec<voom_domain::bad_file::BadFile>> {
            Ok(vec![])
        }
        fn count_bad_files(&self) -> VoomResult<u64> {
            Ok(0)
        }
        fn delete_bad_file(&self, _: &uuid::Uuid) -> VoomResult<()> {
            Ok(())
        }
        fn delete_bad_file_by_path(&self, _: &Path) -> VoomResult<()> {
            Ok(())
        }
        fn vacuum(&self) -> VoomResult<()> {
            Ok(())
        }
        fn prune_missing_files(&self) -> VoomResult<u64> {
            Ok(0)
        }
        fn prune_missing_files_under(&self, _root: &std::path::Path) -> VoomResult<u64> {
            Ok(0)
        }
    }

    fn make_state(auth_token: Option<String>) -> AppState {
        let store = Arc::new(DummyStore);
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
