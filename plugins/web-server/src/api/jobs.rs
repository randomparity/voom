//! Job-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::WebError;
use crate::state::AppState;
use voom_domain::job::{Job, JobStatus};

#[derive(Debug, Deserialize)]
pub struct ListJobsParams {
    pub status: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct JobListResponse {
    pub jobs: Vec<Job>,
}

#[derive(Debug, Serialize)]
pub struct JobStatsResponse {
    pub counts: Vec<JobStatusCount>,
}

#[derive(Debug, Serialize)]
pub struct JobStatusCount {
    pub status: JobStatus,
    pub count: u64,
}

/// GET /api/jobs -- list jobs
pub async fn list_jobs(
    State(state): State<AppState>,
    Query(params): Query<ListJobsParams>,
) -> Result<Json<JobListResponse>, WebError> {
    let store = state.store.clone();
    let status = params.status.as_deref().and_then(JobStatus::parse);
    let limit = params.limit;

    let jobs = tokio::task::spawn_blocking(move || store.list_jobs(status, limit))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    Ok(Json(JobListResponse { jobs }))
}

/// GET /api/jobs/stats -- job counts by status
pub async fn job_stats(State(state): State<AppState>) -> Result<Json<JobStatsResponse>, WebError> {
    let store = state.store.clone();
    let counts = tokio::task::spawn_blocking(move || store.count_jobs_by_status())
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    let counts = counts
        .into_iter()
        .map(|(status, count)| JobStatusCount { status, count })
        .collect();

    Ok(Json(JobStatsResponse { counts }))
}

/// GET /api/jobs/:id -- get a single job
pub async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Job>, WebError> {
    let store = state.store.clone();
    let job = tokio::task::spawn_blocking(move || store.get_job(&id))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    job.map(Json)
        .ok_or_else(|| WebError::NotFound(format!("Job {id} not found")))
}
