//! Policy process-run API endpoints.

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::{WebError, spawn_store_op};
use crate::state::{AppState, ProcessRunLaunchRequest, ProcessRunLaunchResponse};

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct ProcessRunRequest {
    pub paths: Vec<PathBuf>,
    pub policy: Option<PathBuf>,
    pub policy_map: Option<PathBuf>,
    pub estimate_id: Uuid,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub workers: usize,
    #[serde(default)]
    pub force_rescan: bool,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct ProcessRunResponse {
    pub requires_confirmation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<voom_domain::EstimateRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// POST /api/process-runs -- start a confirmed policy run.
#[tracing::instrument(skip(state))]
pub async fn create_process_run(
    State(state): State<AppState>,
    Json(request): Json<ProcessRunRequest>,
) -> Result<Json<ProcessRunResponse>, WebError> {
    validate_request(&request)?;

    let store = state.store.clone();
    let estimate_id = request.estimate_id;
    let estimate = spawn_store_op(move || store.get_estimate_run(&estimate_id)).await?;
    let estimate =
        estimate.ok_or_else(|| WebError::NotFound(format!("Estimate {estimate_id} not found")))?;

    if !request.confirmed {
        return Ok(Json(ProcessRunResponse {
            requires_confirmation: true,
            estimate: Some(estimate),
            run_id: None,
            message: None,
        }));
    }

    let runner = state
        .process_runner
        .as_ref()
        .ok_or_else(|| WebError::Internal("process runner is not configured".to_string()))?;
    let launch_request = ProcessRunLaunchRequest::new(request.paths, estimate_id)
        .with_policy(request.policy)
        .with_policy_map(request.policy_map)
        .with_workers(request.workers)
        .with_force_rescan(request.force_rescan);
    let launched = runner.launch(launch_request).map_err(WebError::Internal)?;

    Ok(Json(response_from_launch(launched)))
}

fn validate_request(request: &ProcessRunRequest) -> Result<(), WebError> {
    if request.paths.is_empty() {
        return Err(WebError::BadRequest(
            "paths must include at least one media path".to_string(),
        ));
    }
    if request.policy.is_some() && request.policy_map.is_some() {
        return Err(WebError::BadRequest(
            "policy and policy_map cannot both be set".to_string(),
        ));
    }
    Ok(())
}

fn response_from_launch(launched: ProcessRunLaunchResponse) -> ProcessRunResponse {
    ProcessRunResponse {
        requires_confirmation: false,
        estimate: None,
        run_id: Some(launched.run_id),
        message: Some(launched.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_run_request_rejects_empty_paths() {
        let request = ProcessRunRequest {
            paths: Vec::new(),
            policy: None,
            policy_map: None,
            estimate_id: Uuid::new_v4(),
            confirmed: false,
            workers: 0,
            force_rescan: false,
        };

        assert!(validate_request(&request).is_err());
    }
}
