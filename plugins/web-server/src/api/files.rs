//! File-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use voom_domain::media::MediaFile;
use voom_domain::storage::FileFilters;

use crate::error::{spawn_store_op, WebError};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListFilesParams {
    pub container: Option<String>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct FileListResponse {
    pub files: Vec<MediaFile>,
    pub total: usize,
}

/// Maximum allowed limit for file listing queries.
const MAX_LIMIT: u32 = 10_000;
/// Maximum allowed offset for file listing queries.
const MAX_OFFSET: u32 = 1_000_000;
/// Maximum length for string filter parameters.
const MAX_FILTER_STRING_LEN: usize = 256;

/// Truncate a string filter to the maximum allowed length.
fn truncate_filter(s: Option<String>) -> Option<String> {
    s.map(|v| {
        if v.len() > MAX_FILTER_STRING_LEN {
            v[..MAX_FILTER_STRING_LEN].to_string()
        } else {
            v
        }
    })
}

/// GET /api/files -- list files with optional filters
#[tracing::instrument(skip(state))]
pub async fn list_files(
    State(state): State<AppState>,
    Query(params): Query<ListFilesParams>,
) -> Result<Json<FileListResponse>, WebError> {
    let store = state.store.clone();
    let count_store = state.store.clone();
    let filters = FileFilters {
        container: truncate_filter(params.container),
        has_codec: truncate_filter(params.codec),
        has_language: truncate_filter(params.language),
        path_prefix: truncate_filter(params.path_prefix),
        limit: Some(params.limit.unwrap_or(100).min(MAX_LIMIT)),
        offset: Some(params.offset.unwrap_or(0).min(MAX_OFFSET)),
    };
    let count_filters = filters.clone();

    let files = spawn_store_op(move || store.list_files(&filters)).await?;
    let total = spawn_store_op(move || count_store.count_files(&count_filters)).await? as usize;

    Ok(Json(FileListResponse { files, total }))
}

/// GET /api/files/:id -- get a single file by ID
#[tracing::instrument(skip(state))]
pub async fn get_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MediaFile>, WebError> {
    let store = state.store.clone();
    let file = spawn_store_op(move || store.get_file(&id)).await?;

    file.map(Json)
        .ok_or_else(|| WebError::NotFound(format!("File {id} not found")))
}

/// DELETE /api/files/:id -- delete a file record
#[tracing::instrument(skip(state))]
pub async fn delete_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, WebError> {
    let store = state.store.clone();
    spawn_store_op(move || store.delete_file(&id)).await?;

    Ok(Json(serde_json::json!({"deleted": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_filter_none_returns_none() {
        assert_eq!(truncate_filter(None), None);
    }

    #[test]
    fn truncate_filter_short_string_unchanged() {
        let input = Some("mkv".to_string());
        assert_eq!(truncate_filter(input), Some("mkv".to_string()));
    }

    #[test]
    fn truncate_filter_at_max_length_unchanged() {
        let s = "a".repeat(MAX_FILTER_STRING_LEN);
        assert_eq!(truncate_filter(Some(s.clone())), Some(s));
    }

    #[test]
    fn truncate_filter_over_max_is_truncated() {
        let s = "b".repeat(MAX_FILTER_STRING_LEN + 100);
        let result = truncate_filter(Some(s));
        let expected = "b".repeat(MAX_FILTER_STRING_LEN);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn list_files_params_deserialize_defaults() {
        let params: ListFilesParams = serde_json::from_str("{}").unwrap();
        assert!(params.container.is_none());
        assert!(params.codec.is_none());
        assert!(params.language.is_none());
        assert!(params.path_prefix.is_none());
        assert!(params.limit.is_none());
        assert!(params.offset.is_none());
    }

    #[test]
    fn list_files_params_deserialize_with_values() {
        let params: ListFilesParams =
            serde_json::from_str(r#"{"container":"mkv","codec":"hevc","limit":50,"offset":10}"#)
                .unwrap();
        assert_eq!(params.container, Some("mkv".to_string()));
        assert_eq!(params.codec, Some("hevc".to_string()));
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.offset, Some(10));
    }

    #[test]
    fn file_list_response_serialization() {
        let response = FileListResponse {
            files: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["files"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }
}
