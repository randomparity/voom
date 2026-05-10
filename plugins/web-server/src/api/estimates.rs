//! Pre-flight estimate API endpoints.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::{WebError, spawn_store_op};
use crate::state::AppState;

const DEFAULT_ESTIMATE_LIMIT: u32 = 25;
const MAX_ESTIMATE_LIMIT: u32 = 500;

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct ListEstimatesParams {
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct EstimateListResponse {
    pub estimates: Vec<voom_domain::EstimateRun>,
    pub total: usize,
}

/// GET /api/estimates -- list persisted pre-flight what-if records.
#[tracing::instrument(skip(state))]
pub async fn list_estimates(
    State(state): State<AppState>,
    Query(params): Query<ListEstimatesParams>,
) -> Result<Json<EstimateListResponse>, WebError> {
    let store = state.store.clone();
    let limit = params
        .limit
        .unwrap_or(DEFAULT_ESTIMATE_LIMIT)
        .min(MAX_ESTIMATE_LIMIT);
    let estimates = spawn_store_op(move || store.list_estimate_runs(limit)).await?;
    let total = estimates.len();

    Ok(Json(EstimateListResponse { estimates, total }))
}

/// GET /api/estimates/:id -- fetch one persisted pre-flight what-if record.
#[tracing::instrument(skip(state))]
pub async fn get_estimate(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<voom_domain::EstimateRun>, WebError> {
    let store = state.store.clone();
    let estimate = spawn_store_op(move || store.get_estimate_run(&id)).await?;

    estimate
        .map(Json)
        .ok_or_else(|| WebError::NotFound(format!("Estimate {id} not found")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_estimates_params_defaults() {
        let params: ListEstimatesParams = serde_json::from_str("{}").unwrap();
        assert!(params.limit.is_none());
    }
}
