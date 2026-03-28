//! Plugin-related API endpoints.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::errors::WebError;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

impl PluginInfo {
    #[must_use]
    pub fn new(name: String, version: String, capabilities: Vec<String>) -> Self {
        Self {
            name,
            version,
            capabilities,
        }
    }
}

#[derive(Debug, Serialize)]
#[non_exhaustive]
pub struct PluginListResponse {
    pub plugins: Vec<PluginInfo>,
    pub total: usize,
}

/// GET /api/plugins -- list registered plugins from the kernel registry snapshot.
#[tracing::instrument(skip(state))]
pub async fn list_plugins(
    State(state): State<AppState>,
) -> Result<Json<PluginListResponse>, WebError> {
    let plugins = state.plugin_info.as_ref().clone();
    let total = plugins.len();
    Ok(Json(PluginListResponse { plugins, total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_info_clone() {
        let info = PluginInfo {
            name: "test".into(),
            version: "1.0.0".into(),
            capabilities: vec!["cap1".into()],
        };
        let cloned = info.clone();
        assert_eq!(cloned.name, "test");
    }

    #[test]
    fn test_plugin_list_response_from_empty_state() {
        let plugins: Vec<PluginInfo> = vec![];
        let total = plugins.len();
        let response = PluginListResponse { plugins, total };
        assert_eq!(response.total, 0);
        assert!(response.plugins.is_empty());
    }

    #[test]
    fn test_plugin_list_response_from_populated_state() {
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
        ];
        let total = plugins.len();
        let response = PluginListResponse { plugins, total };
        assert_eq!(response.total, 2);
        assert_eq!(response.plugins[0].name, "sqlite-store");
    }

    #[test]
    fn test_plugin_info_serialization() {
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
    fn test_plugin_list_response_serialization() {
        let response = PluginListResponse {
            plugins: vec![],
            total: 0,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["plugins"], serde_json::json!([]));
        assert_eq!(json["total"], 0);
    }
}
