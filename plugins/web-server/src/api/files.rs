//! File-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use voom_domain::media::{Container, MediaFile};
use voom_domain::storage::FileFilters;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

/// Shared filter fields used by both API and page handlers.
#[derive(Debug, Default, Clone, Deserialize)]
#[non_exhaustive]
pub struct FileFilterParams {
    pub container: Option<String>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub path_prefix: Option<String>,
}

impl FileFilterParams {
    /// Convert to domain [`FileFilters`] with input truncation.
    ///
    /// Does **not** set `limit` or `offset` — callers must set those separately.
    #[must_use]
    pub fn to_file_filters(&self) -> FileFilters {
        let mut f = FileFilters::default();
        f.container = self.container.as_deref().map(Container::from_extension);
        f.has_codec = truncate_filter(self.codec.clone());
        f.has_language = truncate_filter(self.language.clone());
        f.path_prefix = truncate_filter(self.path_prefix.clone());
        f
    }
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct ListFilesParams {
    #[serde(flatten)]
    pub filters: FileFilterParams,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
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
            match v.char_indices().nth(MAX_FILTER_STRING_LEN) {
                Some((idx, _)) => v[..idx].to_string(),
                None => v,
            }
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
    let mut filters = params.filters.to_file_filters();
    filters.limit = Some(params.limit.unwrap_or(100).min(MAX_LIMIT));
    filters.offset = Some(params.offset.unwrap_or(0).min(MAX_OFFSET));
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
    let file = spawn_store_op(move || store.file(&id)).await?;

    file.map(Json)
        .ok_or_else(|| WebError::NotFound(format!("File {id} not found")))
}

/// DELETE /api/files/:id -- delete a file record
#[tracing::instrument(skip(state))]
pub async fn delete_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeleteResponse>, WebError> {
    let store_check = state.store.clone();
    let store = state.store.clone();

    // Verify the file exists before deleting
    let file = spawn_store_op(move || store_check.file(&id)).await?;
    if file.is_none() {
        return Err(WebError::NotFound(format!("File {id} not found")));
    }

    spawn_store_op(move || store.delete_file(&id)).await?;
    Ok(Json(DeleteResponse { deleted: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_filter_none_returns_none() {
        assert_eq!(truncate_filter(None), None);
    }

    #[test]
    fn test_truncate_filter_short_string_unchanged() {
        let input = Some("mkv".to_string());
        assert_eq!(truncate_filter(input), Some("mkv".to_string()));
    }

    #[test]
    fn test_truncate_filter_at_max_length_unchanged() {
        let s = "a".repeat(MAX_FILTER_STRING_LEN);
        assert_eq!(truncate_filter(Some(s.clone())), Some(s));
    }

    #[test]
    fn test_truncate_filter_over_max_is_truncated() {
        let s = "b".repeat(MAX_FILTER_STRING_LEN + 100);
        let result = truncate_filter(Some(s));
        let expected = "b".repeat(MAX_FILTER_STRING_LEN);
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_list_files_params_deserialize_defaults() {
        let params: ListFilesParams = serde_json::from_str("{}").unwrap();
        assert!(params.filters.container.is_none());
        assert!(params.filters.codec.is_none());
        assert!(params.filters.language.is_none());
        assert!(params.filters.path_prefix.is_none());
        assert!(params.limit.is_none());
        assert!(params.offset.is_none());
    }

    #[test]
    fn test_list_files_params_deserialize_with_values() {
        let params: ListFilesParams =
            serde_json::from_str(r#"{"container":"mkv","codec":"hevc","limit":50,"offset":10}"#)
                .unwrap();
        assert_eq!(params.filters.container, Some("mkv".to_string()));
        assert_eq!(params.filters.codec, Some("hevc".to_string()));
        assert_eq!(params.limit, Some(50));
        assert_eq!(params.offset, Some(10));
    }

    #[test]
    fn test_file_list_response_serialization() {
        let response = FileListResponse {
            files: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["files"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }
}
