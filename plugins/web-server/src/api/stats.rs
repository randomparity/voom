//! Stats-related API endpoints.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use voom_domain::storage::FileFilters;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

use super::jobs::JobStatusCount;

#[derive(Debug, Serialize)]
pub struct DashboardStats {
    pub total_files: usize,
    pub total_jobs: Vec<JobStatusCount>,
}

/// GET /api/stats -- dashboard statistics
pub async fn get_stats(State(state): State<AppState>) -> Result<Json<DashboardStats>, WebError> {
    let store = state.store.clone();
    let store2 = state.store.clone();

    let total_files = spawn_store_op(move || store.count_files(&FileFilters::default())).await?;

    let job_counts = spawn_store_op(move || store2.count_jobs_by_status()).await?;

    Ok(Json(DashboardStats {
        total_files: total_files as usize,
        total_jobs: job_counts
            .into_iter()
            .map(|(status, count)| JobStatusCount { status, count })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::job::JobStatus;

    #[test]
    fn dashboard_stats_serialization() {
        let stats = DashboardStats {
            total_files: 42,
            total_jobs: vec![
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
        let jobs = json["total_jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0]["status"], "pending");
        assert_eq!(jobs[0]["count"], 5);
        assert_eq!(jobs[1]["status"], "completed");
        assert_eq!(jobs[1]["count"], 37);
    }

    #[test]
    fn dashboard_stats_empty() {
        let stats = DashboardStats {
            total_files: 0,
            total_jobs: vec![],
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_files"], 0);
        assert_eq!(json["total_jobs"], serde_json::json!([]));
    }

    #[test]
    fn job_count_serialization() {
        let count = JobStatusCount {
            status: JobStatus::Running,
            count: 3,
        };
        let json = serde_json::to_value(&count).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["count"], 3);
    }
}
