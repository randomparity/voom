//! Plugin-related API endpoints.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::errors::WebError;
use crate::state::AppState;

// Defined here rather than reusing `voom_plugin_sdk::PluginInfoData` because the
// web-server does not depend on voom-plugin-sdk, and adding that dependency just
// for one serializable struct would pull in unnecessary WASM-boundary machinery.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct PluginInfoResponse {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

impl PluginInfoResponse {
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
    pub plugins: Vec<PluginInfoResponse>,
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
        let info = PluginInfoResponse {
            name: "test".into(),
            version: "1.0.0".into(),
            capabilities: vec!["cap1".into()],
        };
        let cloned = info.clone();
        assert_eq!(cloned.name, "test");
    }

    #[test]
    fn test_plugin_list_response_from_empty_state() {
        let plugins: Vec<PluginInfoResponse> = vec![];
        let total = plugins.len();
        let response = PluginListResponse { plugins, total };
        assert_eq!(response.total, 0);
        assert!(response.plugins.is_empty());
    }

    #[test]
    fn test_plugin_list_response_from_populated_state() {
        let plugins = vec![
            PluginInfoResponse {
                name: "sqlite-store".into(),
                version: "0.1.0".into(),
                capabilities: vec!["store".into()],
            },
            PluginInfoResponse {
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
        let info = PluginInfoResponse {
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
