//! Tool detection API endpoints.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::WebError;
use crate::state::AppState;

/// Known tool names that the tool-detector plugin reports.
const KNOWN_TOOLS: &[&str] = &["ffprobe", "ffmpeg", "mkvpropedit", "mkvmerge", "mediainfo"];

#[derive(Debug, Serialize, Deserialize)]
pub struct DetectedTool {
    pub tool_name: String,
    pub version: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<DetectedTool>,
}

/// GET /api/tools -- list detected tools from the store.
pub async fn list_tools(State(state): State<AppState>) -> Result<Json<ToolListResponse>, WebError> {
    let store = state.store.clone();

    let tools = tokio::task::spawn_blocking(move || {
        let mut tools = Vec::new();
        for &tool_name in KNOWN_TOOLS {
            let key = format!("tool:{tool_name}");
            if let Ok(Some(data)) = store.get_plugin_data("tool-detector", &key) {
                if let Ok(tool) = serde_json::from_slice::<DetectedTool>(&data) {
                    tools.push(tool);
                }
            }
        }
        tools
    })
    .await
    .map_err(|e| WebError::Internal(e.to_string()))?;

    Ok(Json(ToolListResponse { tools }))
}
