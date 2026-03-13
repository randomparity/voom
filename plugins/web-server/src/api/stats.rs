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

    let total_files =
        tokio::task::spawn_blocking(move || store.count_files(&FileFilters::default()))
            .await
            .map_err(|e| WebError::Internal(e.to_string()))?
            .map_err(|e| WebError::Storage(e.to_string()))?;

    let job_counts = tokio::task::spawn_blocking(move || store2.count_jobs_by_status())
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    Ok(Json(DashboardStats {
        total_files: total_files as usize,
        total_jobs: job_counts
            .into_iter()
            .map(|(status, count)| JobCount {
                status: format!("{:?}", status),
                count,
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_stats_serialization() {
        let stats = DashboardStats {
            total_files: 42,
            total_jobs: vec![
                JobCount {
                    status: "Pending".into(),
                    count: 5,
                },
                JobCount {
                    status: "Completed".into(),
                    count: 37,
                },
            ],
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["total_files"], 42);
        let jobs = json["total_jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0]["status"], "Pending");
        assert_eq!(jobs[0]["count"], 5);
        assert_eq!(jobs[1]["status"], "Completed");
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
        let count = JobCount {
            status: "Running".into(),
            count: 3,
        };
        let json = serde_json::to_value(&count).unwrap();
        assert_eq!(json["status"], "Running");
        assert_eq!(json["count"], 3);
    }
}
