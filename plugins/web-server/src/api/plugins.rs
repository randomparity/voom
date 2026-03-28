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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub capabilities: Vec<String>,
}

impl PluginInfoResponse {
    #[must_use]
    pub fn new(
        name: String,
        version: String,
        description: String,
        author: String,
        license: String,
        homepage: String,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            name,
            version,
            description,
            author,
            license,
            homepage,
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

    fn test_info(name: &str, caps: Vec<&str>) -> PluginInfoResponse {
        PluginInfoResponse::new(
            name.into(),
            "1.0.0".into(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            caps.into_iter().map(String::from).collect(),
        )
    }

    #[test]
    fn test_plugin_info_clone() {
        let info = test_info("test", vec!["cap1"]);
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
            test_info("sqlite-store", vec!["store"]),
            test_info("discovery", vec!["discover"]),
        ];
        let total = plugins.len();
        let response = PluginListResponse { plugins, total };
        assert_eq!(response.total, 2);
        assert_eq!(response.plugins[0].name, "sqlite-store");
    }

    #[test]
    fn test_plugin_info_serialization() {
        let info = test_info("test-plugin", vec!["cap1", "cap2"]);
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["name"], "test-plugin");
        assert_eq!(json["version"], "1.0.0");
        assert_eq!(json["capabilities"], serde_json::json!(["cap1", "cap2"]));
        // Empty metadata fields should be omitted from JSON
        assert!(json.get("description").is_none());
        assert!(json.get("author").is_none());
    }

    #[test]
    fn test_plugin_info_serialization_with_metadata() {
        let info = PluginInfoResponse::new(
            "test-plugin".into(),
            "1.0.0".into(),
            "A test plugin".into(),
            "Test Author".into(),
            "MIT".into(),
            "https://example.com".into(),
            vec!["cap1".into()],
        );
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["description"], "A test plugin");
        assert_eq!(json["author"], "Test Author");
        assert_eq!(json["license"], "MIT");
        assert_eq!(json["homepage"], "https://example.com");
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
