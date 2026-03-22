//! Job-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;
use voom_domain::job::{Job, JobStatus};
use voom_domain::storage::JobFilters;

/// Maximum allowed limit for job listing queries.
const MAX_JOB_LIMIT: u32 = 10_000;
/// Maximum allowed offset for job listing queries.
const MAX_OFFSET: u32 = 1_000_000;

#[derive(Debug, Deserialize)]
pub struct ListJobsParams {
    pub status: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct JobListResponse {
    pub jobs: Vec<Job>,
    pub total: usize,
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
    let status = match params.status.as_deref() {
        Some(s) => Some(JobStatus::parse(s).ok_or_else(|| {
            WebError::BadRequest(format!(
                "invalid status '{s}': expected one of pending, running, completed, failed, cancelled"
            ))
        })?),
        None => None,
    };
    let limit = params.limit.map(|l| l.min(MAX_JOB_LIMIT));

    let filters = JobFilters {
        status,
        limit,
        offset: params.offset.map(|o| o.min(MAX_OFFSET)),
    };
    let (jobs, total) = spawn_store_op(move || {
        let jobs = store.list_jobs(&filters)?;
        // Compute true total (independent of limit) using count_jobs_by_status
        let counts = store.count_jobs_by_status()?;
        let total = match status {
            Some(s) => counts
                .iter()
                .find(|(st, _)| *st == s)
                .map_or(0, |(_, c)| *c as usize),
            None => counts.iter().map(|(_, c)| *c as usize).sum(),
        };
        Ok((jobs, total))
    })
    .await?;

    Ok(Json(JobListResponse { jobs, total }))
}

/// GET /api/jobs/stats -- job counts by status
#[tracing::instrument(skip(state))]
pub async fn job_stats(State(state): State<AppState>) -> Result<Json<JobStatsResponse>, WebError> {
    let store = state.store.clone();
    let counts = spawn_store_op(move || store.count_jobs_by_status()).await?;

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
    let job = spawn_store_op(move || store.job(&id)).await?;

    job.map(Json)
        .ok_or_else(|| WebError::NotFound(format!("Job {id} not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_jobs_params_deserialize_defaults() {
        let params: ListJobsParams = serde_json::from_str("{}").unwrap();
        assert!(params.status.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn test_list_jobs_params_deserialize_with_values() {
        let params: ListJobsParams =
            serde_json::from_str(r#"{"status":"running","limit":25}"#).unwrap();
        assert_eq!(params.status, Some("running".to_string()));
        assert_eq!(params.limit, Some(25));
    }

    #[test]
    fn test_job_list_response_serialization() {
        let response = JobListResponse {
            jobs: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["jobs"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }

    #[test]
    fn test_job_stats_response_serialization() {
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
    fn test_job_status_count_serialization() {
        let count = JobStatusCount {
            status: JobStatus::Completed,
            count: 42,
        };
        let json = serde_json::to_value(&count).unwrap();
        assert_eq!(json["count"], 42);
    }
}
