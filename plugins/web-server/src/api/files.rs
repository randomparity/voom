//! File-related API endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use voom_domain::media::MediaFile;
use voom_domain::storage::FileFilters;

use crate::error::WebError;
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

/// GET /api/files -- list files with optional filters
pub async fn list_files(
    State(state): State<AppState>,
    Query(params): Query<ListFilesParams>,
) -> Result<Json<FileListResponse>, WebError> {
    let store = state.store.clone();
    let filters = FileFilters {
        container: params.container,
        has_codec: params.codec,
        has_language: params.language,
        path_prefix: params.path_prefix,
        limit: params.limit,
        offset: params.offset,
    };

    let files = tokio::task::spawn_blocking(move || store.list_files(&filters))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    let total = files.len();
    Ok(Json(FileListResponse { files, total }))
}

/// GET /api/files/:id -- get a single file by ID
pub async fn get_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MediaFile>, WebError> {
    let store = state.store.clone();
    let file = tokio::task::spawn_blocking(move || store.get_file(&id))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    file.map(Json)
        .ok_or_else(|| WebError::NotFound(format!("File {id} not found")))
}

/// DELETE /api/files/:id -- delete a file record
pub async fn delete_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, WebError> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || store.delete_file(&id))
        .await
        .map_err(|e| WebError::Internal(e.to_string()))?
        .map_err(|e| WebError::Storage(e.to_string()))?;

    Ok(Json(serde_json::json!({"deleted": true})))
}
