//! Verification and integrity API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use voom_domain::utils::since::parse_since;
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
/// Cutoff (in days) used by `/api/integrity-summary` to compute the `stale` count.
const INTEGRITY_STALE_DAYS: i64 = 30;

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
pub struct VerifyQueryParams {
    pub mode: Option<String>,
    pub outcome: Option<String>,
    pub since: Option<String>,
    pub limit: Option<u32>,
}

/// GET /api/verify -- list verification records with optional filters.
///
/// Query parameters:
/// - `mode` -- one of `quick`, `thorough`, `hash`
/// - `outcome` -- one of `ok`, `warning`, `error`
/// - `since` -- `30d`, `4w`, `12h`, `YYYY-MM-DD`, or `YYYY-MM-DDTHH:MM:SS`
/// - `limit` -- max records to return (default 100, capped at 10_000)
#[tracing::instrument(skip(state))]
pub async fn list_verifications(
    State(state): State<AppState>,
    Query(params): Query<VerifyQueryParams>,
) -> Result<Json<Vec<VerificationRecord>>, WebError> {
    let store = state.store.clone();
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
    let since = params
        .since
        .as_deref()
        .map(parse_since)
        .transpose()
        .map_err(|e| WebError::BadRequest(e.to_string()))?;

    let mut filters = VerificationFilters::default();
    filters.mode = mode;
    filters.outcome = outcome;
    filters.since = since;
    filters.limit = Some(limit);

    let records = spawn_store_op(move || store.list_verifications(&filters)).await?;
    Ok(Json(records))
}

/// GET /api/verify/:file_id -- list verifications for a single file.
#[tracing::instrument(skip(state))]
pub async fn get_file_verifications(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
) -> Result<Json<Vec<VerificationRecord>>, WebError> {
    let store = state.store.clone();
    let mut filters = VerificationFilters::default();
    filters.file_id = Some(file_id);
    filters.limit = Some(DEFAULT_VERIFY_LIMIT);

    let records = spawn_store_op(move || store.list_verifications(&filters)).await?;
    Ok(Json(records))
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
        assert!(params.since.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn verify_query_params_deserialize_with_values() {
        let params: VerifyQueryParams =
            serde_json::from_str(r#"{"mode":"hash","outcome":"ok","since":"7d","limit":50}"#)
                .unwrap();
        assert_eq!(params.mode.as_deref(), Some("hash"));
        assert_eq!(params.outcome.as_deref(), Some("ok"));
        assert_eq!(params.since.as_deref(), Some("7d"));
        assert_eq!(params.limit, Some(50));
    }
}
