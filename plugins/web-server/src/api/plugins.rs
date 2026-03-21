//! Plugin-related API endpoints.

use axum::Json;
use serde::Serialize;

use crate::errors::WebError;

#[derive(Debug, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginListResponse {
    pub plugins: Vec<PluginInfo>,
}

/// GET /api/plugins -- list registered plugins.
/// Note: In a real deployment, this would read from the kernel's registry.
/// For now, returns built-in plugin info.
pub async fn list_plugins() -> Result<Json<PluginListResponse>, WebError> {
    let plugins = vec![
        PluginInfo {
            name: "sqlite-store".into(),
            version: "0.1.0".into(),
            capabilities: vec!["store".into()],
        },
        PluginInfo {
            name: "discovery".into(),
            version: "0.1.0".into(),
            capabilities: vec!["discover".into()],
        },
        PluginInfo {
            name: "ffprobe-introspector".into(),
            version: "0.1.0".into(),
            capabilities: vec!["introspect".into()],
        },
        PluginInfo {
            name: "tool-detector".into(),
            version: "0.1.0".into(),
            capabilities: vec!["detect_tools".into()],
        },
        PluginInfo {
            name: "policy-evaluator".into(),
            version: "0.1.0".into(),
            capabilities: vec!["evaluate".into()],
        },
        PluginInfo {
            name: "phase-orchestrator".into(),
            version: "0.1.0".into(),
            capabilities: vec!["orchestrate".into()],
        },
        PluginInfo {
            name: "mkvtoolnix-executor".into(),
            version: "0.1.0".into(),
            capabilities: vec!["execute".into()],
        },
        PluginInfo {
            name: "ffmpeg-executor".into(),
            version: "0.1.0".into(),
            capabilities: vec!["execute".into()],
        },
        PluginInfo {
            name: "backup-manager".into(),
            version: "0.1.0".into(),
            capabilities: vec!["backup".into()],
        },
        PluginInfo {
            name: "job-manager".into(),
            version: "0.1.0".into(),
            capabilities: vec!["manage_jobs".into()],
        },
        PluginInfo {
            name: "web-server".into(),
            version: "0.1.0".into(),
            capabilities: vec!["serve_http".into()],
        },
    ];
    Ok(Json(PluginListResponse { plugins }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_plugins_returns_all_builtin_plugins() {
        let result = list_plugins().await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert_eq!(response.plugins.len(), 11);
    }

    #[tokio::test]
    async fn list_plugins_contains_expected_names() {
        let result = list_plugins().await.unwrap().0;
        let names: Vec<&str> = result.plugins.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"sqlite-store"));
        assert!(names.contains(&"discovery"));
        assert!(names.contains(&"ffprobe-introspector"));
        assert!(names.contains(&"ffmpeg-executor"));
        assert!(names.contains(&"mkvtoolnix-executor"));
        assert!(names.contains(&"web-server"));
        assert!(names.contains(&"job-manager"));
        assert!(names.contains(&"backup-manager"));
    }

    #[tokio::test]
    async fn list_plugins_all_have_version() {
        let result = list_plugins().await.unwrap().0;
        for plugin in &result.plugins {
            assert_eq!(plugin.version, "0.1.0");
        }
    }

    #[tokio::test]
    async fn list_plugins_all_have_capabilities() {
        let result = list_plugins().await.unwrap().0;
        for plugin in &result.plugins {
            assert!(
                !plugin.capabilities.is_empty(),
                "Plugin {} has no capabilities",
                plugin.name
            );
        }
    }

    #[test]
    fn plugin_info_serialization() {
        let info = PluginInfo {
            name: "test-plugin".into(),
            version: "1.0.0".into(),
            capabilities: vec!["cap1".into(), "cap2".into()],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["name"], "test-plugin");
        assert_eq!(json["version"], "1.0.0");
        assert_eq!(json["capabilities"], serde_json::json!(["cap1", "cap2"]));
    }

    #[test]
    fn plugin_list_response_serialization() {
        let response = PluginListResponse { plugins: vec![] };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["plugins"], serde_json::json!([]));
    }
}
