//! Verification and integrity API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use voom_domain::verification::{
    IntegritySummary, VerificationFilters, VerificationMode, VerificationOutcome,
    VerificationRecord,
};

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

/// Default page size when the caller does not specify `limit`.
const DEFAULT_VERIFY_LIMIT: u32 = 100;
/// Maximum allowed limit for verification listing queries.
const MAX_VERIFY_LIMIT: u32 = 10_000;
/// Maximum allowed offset for verification listing queries.
const MAX_VERIFY_OFFSET: u32 = 1_000_000;
/// Cutoff (in days) used by `/api/integrity-summary` to compute the `stale` count.
const INTEGRITY_STALE_DAYS: i64 = 30;

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
pub struct VerifyQueryParams {
    pub mode: Option<String>,
    pub outcome: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct VerificationListResponse {
    pub records: Vec<VerificationRecord>,
    pub total: usize,
}

/// GET /api/verify -- list verification records with optional filters.
///
/// Query parameters:
/// - `mode` -- one of `quick`, `thorough`, `hash`
/// - `outcome` -- one of `ok`, `warning`, `error`
/// - `limit` -- max records to return (default 100, capped at 10_000)
/// - `offset` -- records to skip (default 0, capped at 1_000_000)
///
/// `since`-style filtering is not yet exposed via the web API; see issue #196.
#[tracing::instrument(skip(state))]
pub async fn list_verifications(
    State(state): State<AppState>,
    Query(params): Query<VerifyQueryParams>,
) -> Result<Json<VerificationListResponse>, WebError> {
    let store = state.store.clone();
    let count_store = state.store.clone();
    let mode = match params.mode.as_deref() {
        Some(s) => Some(VerificationMode::parse(s).ok_or_else(|| {
            WebError::BadRequest(format!(
                "invalid mode '{s}': expected one of quick, thorough, hash"
            ))
        })?),
        None => None,
    };
    let outcome = match params.outcome.as_deref() {
        Some(s) => Some(VerificationOutcome::parse(s).ok_or_else(|| {
            WebError::BadRequest(format!(
                "invalid outcome '{s}': expected one of ok, warning, error"
            ))
        })?),
        None => None,
    };
    let limit = params
        .limit
        .map_or(DEFAULT_VERIFY_LIMIT, |l| l.min(MAX_VERIFY_LIMIT));

    let mut filters = VerificationFilters::default();
    filters.mode = mode;
    filters.outcome = outcome;
    filters.limit = Some(limit);
    filters.offset = Some(params.offset.unwrap_or(0).min(MAX_VERIFY_OFFSET));
    let mut count_filters = filters.clone();
    count_filters.limit = None;
    count_filters.offset = None;

    let records = spawn_store_op(move || store.list_verifications(&filters)).await?;
    let total = spawn_store_op(move || count_store.list_verifications(&count_filters))
        .await?
        .len();
    Ok(Json(VerificationListResponse { records, total }))
}

/// GET /api/verify/:file_id -- list verifications for a single file.
#[tracing::instrument(skip(state))]
pub async fn get_file_verifications(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Json<VerificationListResponse>, WebError> {
    let store = state.store.clone();
    let count_store = state.store.clone();
    let mut filters = VerificationFilters::default();
    filters.file_id = Some(file_id);
    filters.limit = Some(DEFAULT_VERIFY_LIMIT);
    filters.offset = Some(0);
    let mut count_filters = filters.clone();
    count_filters.limit = None;
    count_filters.offset = None;

    let records = spawn_store_op(move || store.list_verifications(&filters)).await?;
    let total = spawn_store_op(move || count_store.list_verifications(&count_filters))
        .await?
        .len();
    Ok(Json(VerificationListResponse { records, total }))
}

/// GET /api/integrity-summary -- aggregate integrity counts.
///
/// Files last verified more than 30 days ago are counted as stale.
#[tracing::instrument(skip(state))]
pub async fn integrity_summary(
    State(state): State<AppState>,
) -> Result<Json<IntegritySummary>, WebError> {
    let store = state.store.clone();
    let since = chrono::Utc::now() - chrono::Duration::days(INTEGRITY_STALE_DAYS);
    let summary = spawn_store_op(move || store.integrity_summary(since)).await?;
    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_query_params_deserialize_defaults() {
        let params: VerifyQueryParams = serde_json::from_str("{}").unwrap();
        assert!(params.mode.is_none());
        assert!(params.outcome.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn verify_query_params_deserialize_with_values() {
        let params: VerifyQueryParams =
            serde_json::from_str(r#"{"mode":"hash","outcome":"ok","limit":50}"#).unwrap();
        assert_eq!(params.mode.as_deref(), Some("hash"));
        assert_eq!(params.outcome.as_deref(), Some("ok"));
        assert_eq!(params.limit, Some(50));
        assert!(params.offset.is_none());
    }
}
