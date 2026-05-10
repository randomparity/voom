//! File transition history API endpoint.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use uuid::Uuid;

use voom_domain::transition::FileTransition;

use crate::errors::{WebError, spawn_store_op};
use crate::state::AppState;

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct TransitionsResponse {
    pub transitions: Vec<FileTransition>,
}

/// GET /api/files/:id/transitions -- list transitions for a file
#[tracing::instrument(skip(state))]
pub async fn list_transitions(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TransitionsResponse>, WebError> {
    let store = state.store.clone();

    let (file, transitions) = spawn_store_op(move || {
        let file = store.file(&id)?;
        let transitions = store.transitions_for_file(&id)?;
        Ok((file, transitions))
    })
    .await?;

    if file.is_none() {
        return Err(WebError::NotFound(format!("File {id} not found")));
    }

    Ok(Json(TransitionsResponse { transitions }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transitions_response_serialization() {
        let resp = TransitionsResponse {
            transitions: vec![],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["transitions"], serde_json::json!([]));
    }
}
