//! Tool detection API endpoints.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::errors::{spawn_store_op, WebError};
use crate::state::AppState;

/// Known tool names that the tool-detector plugin reports.
const KNOWN_TOOLS: &[&str] = &[
    "ffprobe",
    "ffmpeg",
    "mkvmerge",
    "mkvpropedit",
    "mkvextract",
    "mediainfo",
    "HandBrakeCLI",
];

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DetectedTool {
    pub name: String,
    pub version: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct ToolListResponse {
    pub tools: Vec<DetectedTool>,
    pub total: usize,
}

/// GET /api/tools -- list detected tools from the store.
#[tracing::instrument(skip(state))]
pub async fn list_tools(State(state): State<AppState>) -> Result<Json<ToolListResponse>, WebError> {
    let store = state.store.clone();

    let tools = spawn_store_op(move || {
        let mut tools = Vec::new();
        for &tool_name in KNOWN_TOOLS {
            let key = format!("tool:{tool_name}");
            if let Some(data) = store.plugin_data("tool-detector", &key)? {
                if let Ok(tool) = serde_json::from_slice::<DetectedTool>(&data) {
                    tools.push(tool);
                }
            }
        }
        Ok(tools)
    })
    .await?;

    let total = tools.len();
    Ok(Json(ToolListResponse { tools, total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_tools_contains_expected_names() {
        assert!(KNOWN_TOOLS.contains(&"ffprobe"));
        assert!(KNOWN_TOOLS.contains(&"ffmpeg"));
        assert!(KNOWN_TOOLS.contains(&"mkvpropedit"));
        assert!(KNOWN_TOOLS.contains(&"mkvmerge"));
        assert!(KNOWN_TOOLS.contains(&"mkvextract"));
        assert!(KNOWN_TOOLS.contains(&"mediainfo"));
        assert!(KNOWN_TOOLS.contains(&"HandBrakeCLI"));
        assert_eq!(KNOWN_TOOLS.len(), 7);
    }

    #[test]
    fn test_detected_tool_serialization() {
        let tool = DetectedTool {
            name: "ffprobe".into(),
            version: "6.1".into(),
            path: "/usr/bin/ffprobe".into(),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "ffprobe");
        assert_eq!(json["version"], "6.1");
        assert_eq!(json["path"], "/usr/bin/ffprobe");
    }

    #[test]
    fn test_detected_tool_deserialization() {
        let json = r#"{"name":"ffmpeg","version":"7.0","path":"/usr/local/bin/ffmpeg"}"#;
        let tool: DetectedTool = serde_json::from_str(json).unwrap();
        assert_eq!(tool.name, "ffmpeg");
        assert_eq!(tool.version, "7.0");
        assert_eq!(tool.path, "/usr/local/bin/ffmpeg");
    }

    #[test]
    fn test_tool_list_response_serialization() {
        let response = ToolListResponse {
            tools: vec![DetectedTool {
                name: "mkvmerge".into(),
                version: "81.0".into(),
                path: "/usr/bin/mkvmerge".into(),
            }],
            total: 1,
        };
        let json = serde_json::to_value(&response).unwrap();
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "mkvmerge");
        assert_eq!(json["total"], 1);
    }

    #[test]
    fn test_tool_list_response_empty() {
        let response = ToolListResponse {
            tools: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["tools"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }
}
