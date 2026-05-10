//! Tool detection API endpoints.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::errors::{WebError, spawn_store_op};
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
    "hdr10plus_tool",
    "dovi_tool",
];

/// Known executor plugin names that emit capability events.
const KNOWN_EXECUTORS: &[&str] = &["ffmpeg-executor", "mkvtoolnix-executor"];

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

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExecutorCapabilitiesResponse {
    pub plugin_name: String,
    pub codecs: CodecCapabilitiesDto,
    pub formats: Vec<String>,
    pub hw_accels: Vec<String>,
    #[serde(default)]
    pub parallel_limits: Vec<voom_domain::events::ExecutorParallelLimit>,
}

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CodecCapabilitiesDto {
    pub decoders: Vec<String>,
    pub encoders: Vec<String>,
    #[serde(default)]
    pub hw_decoders: Vec<String>,
    #[serde(default)]
    pub hw_encoders: Vec<String>,
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct ExecutorCapabilitiesListResponse {
    pub executors: Vec<ExecutorCapabilitiesResponse>,
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

/// GET /api/executor-capabilities -- list probed executor capabilities.
#[tracing::instrument(skip(state))]
pub async fn list_executor_capabilities(
    State(state): State<AppState>,
) -> Result<Json<ExecutorCapabilitiesListResponse>, WebError> {
    let store = state.store.clone();

    let executors = spawn_store_op(move || {
        let mut executors = Vec::new();
        for &name in KNOWN_EXECUTORS {
            let key = format!("executor_capabilities:{name}");
            if let Some(data) = store.plugin_data(name, &key)? {
                if let Ok(caps) = serde_json::from_slice::<ExecutorCapabilitiesResponse>(&data) {
                    executors.push(caps);
                }
            }
        }
        Ok(executors)
    })
    .await?;

    let total = executors.len();
    Ok(Json(ExecutorCapabilitiesListResponse { executors, total }))
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
        assert!(KNOWN_TOOLS.contains(&"hdr10plus_tool"));
        assert!(KNOWN_TOOLS.contains(&"dovi_tool"));
        assert_eq!(KNOWN_TOOLS.len(), 9);
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

    #[test]
    fn test_executor_capabilities_response_serialization() {
        let response = ExecutorCapabilitiesResponse {
            plugin_name: "ffmpeg-executor".into(),
            codecs: CodecCapabilitiesDto {
                decoders: vec!["h264".into(), "hevc".into()],
                encoders: vec!["libx264".into()],
                hw_decoders: vec![],
                hw_encoders: vec!["h264_nvenc".into()],
            },
            formats: vec!["matroska".into(), "mp4".into()],
            hw_accels: vec!["videotoolbox".into()],
            parallel_limits: vec![voom_domain::events::ExecutorParallelLimit::new(
                "hw:nvenc", 4,
            )],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["plugin_name"], "ffmpeg-executor");
        assert_eq!(json["codecs"]["decoders"][0], "h264");
        assert_eq!(json["formats"][0], "matroska");
        assert_eq!(json["hw_accels"][0], "videotoolbox");
        assert_eq!(json["parallel_limits"][0]["resource"], "hw:nvenc");
        assert_eq!(json["parallel_limits"][0]["max_parallel"], 4);
    }

    #[test]
    fn test_executor_capabilities_deserialization_from_domain_event() {
        // Verify that domain ExecutorCapabilitiesEvent JSON deserializes
        // into our DTO (same shape).
        let domain_event = voom_domain::events::ExecutorCapabilitiesEvent::new(
            "ffmpeg-executor",
            voom_domain::events::CodecCapabilities::new(vec!["aac".into()], vec!["aac".into()]),
            vec!["mp4".into()],
            vec![],
        )
        .with_parallel_limits(vec![voom_domain::events::ExecutorParallelLimit::new(
            "hw:nvenc", 4,
        )]);
        let bytes = serde_json::to_vec(&domain_event).unwrap();
        let dto: ExecutorCapabilitiesResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(dto.plugin_name, "ffmpeg-executor");
        assert_eq!(dto.codecs.decoders, vec!["aac"]);
        assert_eq!(dto.parallel_limits.len(), 1);
        assert_eq!(dto.parallel_limits[0].resource, "hw:nvenc");
    }

    #[test]
    fn test_executor_capabilities_list_response() {
        let response = ExecutorCapabilitiesListResponse {
            executors: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["executors"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }
}
