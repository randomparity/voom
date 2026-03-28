//! Health check API endpoints.

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct HealthResponse {
    pub status: &'static str,
    pub checks: Vec<HealthCheckSummary>,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct HealthCheckSummary {
    pub check_name: String,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// GET /api/health -- latest result per check.
#[tracing::instrument(skip(state))]
pub async fn get_health(State(state): State<AppState>) -> Result<Json<HealthResponse>, WebError> {
    let store = state.store.clone();

    let checks = spawn_store_op(move || store.latest_health_checks()).await?;

    let all_passed = checks.iter().all(|c| c.passed);
    let summaries: Vec<HealthCheckSummary> = checks
        .into_iter()
        .map(|c| HealthCheckSummary {
            check_name: c.check_name,
            passed: c.passed,
            details: c.details,
            checked_at: c.checked_at,
        })
        .collect();

    Ok(Json(HealthResponse {
        status: if all_passed { "healthy" } else { "degraded" },
        checks: summaries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_response_serialization_healthy() {
        let response = HealthResponse {
            status: "healthy",
            checks: vec![HealthCheckSummary {
                check_name: "data_dir_exists".into(),
                passed: true,
                details: Some("/data/voom".into()),
                checked_at: Utc::now(),
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["status"], "healthy");
        let checks = json["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["check_name"], "data_dir_exists");
        assert_eq!(checks[0]["passed"], true);
    }

    #[test]
    fn test_health_response_serialization_degraded() {
        let response = HealthResponse {
            status: "degraded",
            checks: vec![HealthCheckSummary {
                check_name: "data_dir_writable".into(),
                passed: false,
                details: Some("permission denied".into()),
                checked_at: Utc::now(),
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["status"], "degraded");
        assert_eq!(json["checks"][0]["passed"], false);
    }

    #[test]
    fn test_health_response_empty_checks() {
        let response = HealthResponse {
            status: "healthy",
            checks: vec![],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["checks"], serde_json::json!([]));
    }

    #[test]
    fn test_health_check_summary_skips_none_details() {
        let summary = HealthCheckSummary {
            check_name: "test".into(),
            passed: true,
            details: None,
            checked_at: Utc::now(),
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert!(!json.as_object().unwrap().contains_key("details"));
    }
}
