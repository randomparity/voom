//! Plugin-related API endpoints.

use axum::Json;
use serde::Serialize;

use crate::error::WebError;

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
