//! File transition history API endpoint.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use voom_domain::transition::FileTransition;

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

#[derive(Debug, Serialize)]
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

    let transitions = spawn_store_op(move || store.transitions_for_file(&id)).await?;

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
