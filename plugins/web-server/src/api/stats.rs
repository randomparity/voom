//! Stats-related API endpoints.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use voom_domain::storage::FileFilters;

use crate::error::WebError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct DashboardStats {
    pub total_files: usize,
    pub total_jobs: Vec<JobCount>,
}

#[derive(Debug, Serialize)]
pub struct JobCount {
    pub status: String,
    pub count: u64,
}

/// GET /api/stats -- dashboard statistics
pub async fn get_stats(State(state): State<AppState>) -> Result<Json<DashboardStats>, WebError> {
    let store = state.store.clone();
    let store2 = state.store.clone();

    let files = tokio::task::spawn_blocking(move || store.list_files(&FileFilters::default()))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    let job_counts = tokio::task::spawn_blocking(move || store2.count_jobs_by_status())
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    Ok(Json(DashboardStats {
        total_files: files.len(),
        total_jobs: job_counts
            .into_iter()
            .map(|(status, count)| JobCount {
                status: format!("{:?}", status),
                count,
            })
            .collect(),
    }))
}
