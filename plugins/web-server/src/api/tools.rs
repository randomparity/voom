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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tools_contains_expected_names() {
        assert!(KNOWN_TOOLS.contains(&"ffprobe"));
        assert!(KNOWN_TOOLS.contains(&"ffmpeg"));
        assert!(KNOWN_TOOLS.contains(&"mkvpropedit"));
        assert!(KNOWN_TOOLS.contains(&"mkvmerge"));
        assert!(KNOWN_TOOLS.contains(&"mediainfo"));
        assert_eq!(KNOWN_TOOLS.len(), 5);
    }

    #[test]
    fn detected_tool_serialization() {
        let tool = DetectedTool {
            tool_name: "ffprobe".into(),
            version: "6.1".into(),
            path: "/usr/bin/ffprobe".into(),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["tool_name"], "ffprobe");
        assert_eq!(json["version"], "6.1");
        assert_eq!(json["path"], "/usr/bin/ffprobe");
    }

    #[test]
    fn detected_tool_deserialization() {
        let json = r#"{"tool_name":"ffmpeg","version":"7.0","path":"/usr/local/bin/ffmpeg"}"#;
        let tool: DetectedTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.tool_name, "ffmpeg");
        assert_eq!(tool.version, "7.0");
        assert_eq!(tool.path, "/usr/local/bin/ffmpeg");
    }

    #[test]
    fn tool_list_response_serialization() {
        let response = ToolListResponse {
            tools: vec![DetectedTool {
                tool_name: "mkvmerge".into(),
                version: "81.0".into(),
                path: "/usr/bin/mkvmerge".into(),
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["tool_name"], "mkvmerge");
    }

    #[test]
    fn tool_list_response_empty() {
        let response = ToolListResponse { tools: vec![] };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["tools"], serde_json::json!([]));
    }
}
