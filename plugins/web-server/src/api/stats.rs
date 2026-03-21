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

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::job::JobStatus;

    #[test]
    fn dashboard_stats_serialization() {
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
    fn dashboard_stats_empty() {
        let stats = DashboardStats {
            total_files: 0,
            job_status_counts: vec![],
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_files"], 0);
        assert_eq!(json["job_status_counts"], serde_json::json!([]));
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
