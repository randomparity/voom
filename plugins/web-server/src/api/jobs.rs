//! Job-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::WebError;
use crate::state::AppState;
use voom_domain::job::{Job, JobStatus};
use voom_domain::storage::JobFilters;

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
#[tracing::instrument(skip(state))]
pub async fn list_jobs(
    State(state): State<AppState>,
    Query(params): Query<ListJobsParams>,
) -> Result<Json<JobListResponse>, WebError> {
    let store = state.store.clone();
    let status = params.status.as_deref().and_then(JobStatus::parse);
    let limit = params.limit;

    let filters = JobFilters { status, limit };
    let jobs = tokio::task::spawn_blocking(move || store.list_jobs(&filters))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    Ok(Json(JobListResponse { jobs }))
}

/// GET /api/jobs/stats -- job counts by status
#[tracing::instrument(skip(state))]
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
#[tracing::instrument(skip(state))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_jobs_params_deserialize_defaults() {
        let params: ListJobsParams = serde_json::from_str("{}").unwrap();
        assert!(params.status.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn list_jobs_params_deserialize_with_values() {
        let params: ListJobsParams =
            serde_json::from_str(r#"{"status":"running","limit":25}"#).unwrap();
        assert_eq!(params.status, Some("running".to_string()));
        assert_eq!(params.limit, Some(25));
    }

    #[test]
    fn job_list_response_serialization() {
        let response = JobListResponse { jobs: vec![] };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["jobs"], serde_json::json!([]));
    }

    #[test]
    fn job_stats_response_serialization() {
        let response = JobStatsResponse {
            counts: vec![
                JobStatusCount {
                    status: JobStatus::Pending,
                    count: 5,
                },
                JobStatusCount {
                    status: JobStatus::Running,
                    count: 2,
                },
            ],
        };
        let json = serde_json::to_value(&response).unwrap();
        let counts = json["counts"].as_array().unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0]["count"], 5);
        assert_eq!(counts[1]["count"], 2);
    }

    #[test]
    fn job_status_count_serialization() {
        let count = JobStatusCount {
            status: JobStatus::Completed,
            count: 42,
        };
        let json = serde_json::to_value(&count).unwrap();
        assert_eq!(json["count"], 42);
    }
}
