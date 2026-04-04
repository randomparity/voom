//! Stats-related API endpoints.

use axum::extract::{Query, State};
use axum::Json;
use serde::Serialize;

use voom_domain::stats::{LibrarySnapshot, SnapshotTrigger};
use voom_domain::storage::FileFilters;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

use super::jobs::JobStatusCount;

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct DashboardStats {
    pub total_files: usize,
    pub job_status_counts: Vec<JobStatusCount>,
}

/// GET /api/stats -- dashboard statistics
#[tracing::instrument(skip(state))]
pub async fn get_stats(State(state): State<AppState>) -> Result<Json<DashboardStats>, WebError> {
    let store = state.store.clone();

    let (total_files, job_counts) = spawn_store_op(move || {
        let total_files = store.count_files(&FileFilters::default())?;
        let job_counts = store.count_jobs_by_status()?;
        Ok((total_files, job_counts))
    })
    .await?;

    Ok(Json(DashboardStats {
        total_files: total_files as usize,
        job_status_counts: job_counts
            .into_iter()
            .map(|(status, count)| JobStatusCount { status, count })
            .collect(),
    }))
}

/// GET /api/stats/library — live library statistics
#[tracing::instrument(skip(state))]
pub async fn get_library_stats(
    State(state): State<AppState>,
) -> Result<Json<LibrarySnapshot>, WebError> {
    let store = state.store.clone();
    let snapshot =
        spawn_store_op(move || store.gather_library_stats(SnapshotTrigger::Manual)).await?;
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
    use voom_domain::job::JobStatus;

    #[test]
    fn test_dashboard_stats_serialization() {
        let stats = DashboardStats {
            total_files: 42,
            job_status_counts: vec![
                JobStatusCount {
                    status: JobStatus::Pending,
                    count: 5,
                },
                JobStatusCount {
                    status: JobStatus::Completed,
                    count: 37,
                },
            ],
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_files"], 42);
        let jobs = json["job_status_counts"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0]["status"], "pending");
        assert_eq!(jobs[0]["count"], 5);
        assert_eq!(jobs[1]["status"], "completed");
        assert_eq!(jobs[1]["count"], 37);
    }

    #[test]
    fn test_dashboard_stats_empty() {
        let stats = DashboardStats {
            total_files: 0,
            job_status_counts: vec![],
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_files"], 0);
        assert_eq!(json["job_status_counts"], serde_json::json!([]));
    }

    #[test]
    fn test_job_count_serialization() {
        let count = JobStatusCount {
            status: JobStatus::Running,
            count: 3,
        };
        let json = serde_json::to_value(&count).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["count"], 3);
    }

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
