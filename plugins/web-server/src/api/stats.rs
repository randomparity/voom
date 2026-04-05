//! Stats-related API endpoints.

use axum::extract::{Query, State};
use axum::Json;

use voom_domain::errors::VoomError;
use voom_domain::stats::LibrarySnapshot;
use voom_report::{ReportPlugin, ReportRequest, ReportSection};

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

/// GET /api/stats -- dashboard statistics
#[tracing::instrument(skip(state))]
pub async fn get_stats(State(state): State<AppState>) -> Result<Json<serde_json::Value>, WebError> {
    let store = state.store.clone();
    let result = spawn_store_op(move || {
        let request = ReportRequest::summary();
        ReportPlugin::query(store.as_ref(), &request).map_err(|e| VoomError::Other(e.into()))
    })
    .await?;

    let library = result.library.as_ref();
    let total_files = library.map_or(0, |s| s.files.total_count);
    let job_counts: Vec<serde_json::Value> = library
        .map(|s| {
            s.jobs
                .by_status
                .iter()
                .map(|(status, count)| serde_json::json!({"status": status, "count": count}))
                .collect()
        })
        .unwrap_or_default();

    Ok(Json(serde_json::json!({
        "total_files": total_files,
        "job_status_counts": job_counts,
    })))
}

/// GET /api/stats/library — live library statistics
#[tracing::instrument(skip(state))]
pub async fn get_library_stats(
    State(state): State<AppState>,
) -> Result<Json<LibrarySnapshot>, WebError> {
    let store = state.store.clone();
    let result = spawn_store_op(move || {
        let request = ReportRequest::new(vec![ReportSection::Library]);
        ReportPlugin::query(store.as_ref(), &request).map_err(|e| VoomError::Other(e.into()))
    })
    .await?;

    let snapshot = result
        .library
        .ok_or_else(|| WebError::Internal("library section missing from report".into()))?;
    Ok(Json(snapshot))
}

#[derive(Debug, serde::Deserialize)]
pub struct HistoryParams {
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

/// GET /api/stats/history?limit=20 — snapshot history
#[tracing::instrument(skip(state))]
pub async fn get_stats_history(
    State(state): State<AppState>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<Vec<LibrarySnapshot>>, WebError> {
    let store = state.store.clone();
    let snapshots = spawn_store_op(move || store.list_snapshots(params.limit)).await?;
    Ok(Json(snapshots))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_params_default_limit() {
        let params: HistoryParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.limit, 20);
    }

    #[test]
    fn test_history_params_explicit_limit() {
        let params: HistoryParams = serde_json::from_str(r#"{"limit": 50}"#).unwrap();
        assert_eq!(params.limit, 50);
    }
}
