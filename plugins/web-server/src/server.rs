//! Server startup and configuration.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use tokio::sync::broadcast;
use voom_domain::storage::StorageTrait;

use crate::errors::ServerError;
use crate::router::build_router;
use crate::state::{AppState, SseEvent};

/// Configuration for the web server.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub template_dir: Option<String>,
    pub auth_token: Option<String>,
    pub plugin_info: Vec<crate::api::plugins::PluginInfoResponse>,
    pub data_dir: Option<std::path::PathBuf>,
    /// SSE broadcast sender supplied by the caller. All channel creation is
    /// the caller's responsibility; the server uses this sender directly so
    /// that it shares the same channel as any kernel-side bridge plugin.
    pub sse_tx: broadcast::Sender<SseEvent>,
}

impl ServerConfig {
    #[must_use]
    pub fn new(host: String, port: u16, sse_tx: broadcast::Sender<SseEvent>) -> Self {
        Self {
            host,
            port,
            template_dir: None,
            auth_token: None,
            plugin_info: Vec::new(),
            data_dir: None,
            sse_tx,
        }
    }
}

/// Start the web server.
///
/// The `shutdown` future is awaited for graceful shutdown (e.g. CTRL-C).
pub async fn start_server(
    config: ServerConfig,
    store: Arc<dyn StorageTrait>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), ServerError> {
    match &config.auth_token {
        None => {
            tracing::warn!(
                "Web server starting without authentication \
                 — all requests will be allowed"
            );
        }
        Some(token) if token.len() < 32 => {
            tracing::warn!(
                "Auth token is short ({} chars); consider using \
                 a stronger token: openssl rand -base64 32",
                token.len()
            );
        }
        Some(_) => {}
    }

    let templates = load_templates(config.template_dir.as_deref())?;
    let state = AppState::new(
        store,
        config.sse_tx,
        templates,
        config.auth_token,
        config.data_dir,
    )
    .with_plugin_info(config.plugin_info);
    let router = build_router(state).layer(DefaultBodyLimit::max(2 * 1024 * 1024)); // 2 MiB

    let address = format!("{}:{}", config.host, config.port);
    let addr: SocketAddr = address
        .parse()
        .map_err(|e| ServerError::InvalidBindAddress {
            address: address.clone(),
            source: e,
        })?;

    tracing::info!("Web server listening on http://{}", addr);

    let listener =
        tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ServerError::BindFailed {
                address: address.clone(),
                source: e,
            })?;

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| ServerError::Serve { source: e })?;

    Ok(())
}

fn load_templates(template_dir: Option<&str>) -> Result<tera::Tera, ServerError> {
    let dir = template_dir.unwrap_or("web/templates");

    // Try to load from disk first
    let pattern = format!("{dir}/**/*.html");
    match tera::Tera::new(&pattern) {
        Ok(t) if !t.get_template_names().collect::<Vec<_>>().is_empty() => {
            tracing::info!(dir = dir, "Loaded templates from disk");
            Ok(t)
        }
        _ => {
            tracing::info!("Using embedded templates");
            embedded_templates()
        }
    }
}

/// Embedded templates as fallback when web/templates/ doesn't exist on disk.
///
/// Public so that integration tests and other crates can obtain the same
/// template set without starting the full server.
pub fn embedded_templates() -> Result<tera::Tera, ServerError> {
    macro_rules! register_templates {
        ($tera:expr, $( $name:literal ),+ $(,)?) => {
            $(
                $tera
                    .add_raw_template(
                        $name,
                        include_str!(concat!("../templates/", $name)),
                    )
                    .map_err(|e| ServerError::Template(
                        format!("failed to add template {}: {e}", $name)
                    ))?;
            )+
        };
    }

    let mut tera = tera::Tera::default();

    register_templates!(
        tera,
        "base.html",
        "dashboard.html",
        "library.html",
        "file_detail.html",
        "integrity.html",
        "policies.html",
        "policy_editor.html",
        "jobs.html",
        "plugins.html",
        "settings.html",
    );

    Ok(tera)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_fields() {
        let (sse_tx, _rx) = broadcast::channel::<SseEvent>(1);
        let config = ServerConfig {
            host: "127.0.0.1".into(),
            port: 8080,
            template_dir: None,
            auth_token: Some("secret".into()),
            plugin_info: vec![],
            data_dir: None,
            sse_tx,
        };
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8080);
        assert!(config.template_dir.is_none());
        assert_eq!(config.auth_token, Some("secret".to_string()));
    }

    #[test]
    fn test_server_config_clone() {
        let (sse_tx, _rx) = broadcast::channel::<SseEvent>(1);
        let config = ServerConfig {
            host: "0.0.0.0".into(),
            port: 3000,
            template_dir: Some("/tmp/templates".into()),
            auth_token: None,
            plugin_info: vec![],
            data_dir: None,
            sse_tx,
        };
        let cloned = config.clone();
        assert_eq!(cloned.host, "0.0.0.0");
        assert_eq!(cloned.port, 3000);
        assert_eq!(cloned.template_dir, Some("/tmp/templates".to_string()));
        assert!(cloned.auth_token.is_none());
    }

    #[test]
    fn test_server_config_stores_sse_sender() {
        let (tx, mut rx) = broadcast::channel::<SseEvent>(8);
        let config = ServerConfig::new("127.0.0.1".into(), 8080, tx);
        config
            .sse_tx
            .send(SseEvent::JobStarted {
                job_id: "test".into(),
                description: "test".into(),
            })
            .expect("send should succeed with a live receiver");
        match rx.try_recv() {
            Ok(SseEvent::JobStarted {
                job_id,
                description,
            }) => {
                assert_eq!(job_id, "test");
                assert_eq!(description, "test");
            }
            other => panic!("expected JobStarted, got {other:?}"),
        }
    }

    #[test]
    fn test_embedded_templates_contains_all_expected_templates() {
        let tera = embedded_templates().unwrap();
        let names: Vec<&str> = tera.get_template_names().collect();
        let expected = [
            "base.html",
            "dashboard.html",
            "library.html",
            "file_detail.html",
            "integrity.html",
            "policies.html",
            "policy_editor.html",
            "jobs.html",
            "plugins.html",
            "settings.html",
        ];
        for name in &expected {
            assert!(names.contains(name), "Missing template: {name}");
        }
    }

    #[test]
    fn test_embedded_templates_returns_same_as_direct_call() {
        let tera = embedded_templates().unwrap();
        let names: Vec<&str> = tera.get_template_names().collect();
        assert!(names.contains(&"dashboard.html"));
        assert!(names.contains(&"base.html"));
    }

    #[test]
    fn test_load_templates_falls_back_to_embedded_when_dir_missing() {
        // Using a non-existent directory should fall back to embedded
        let result = load_templates(Some("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_ok());
        let tera = result.unwrap();
        let names: Vec<&str> = tera.get_template_names().collect();
        assert!(names.contains(&"dashboard.html"));
    }

    #[test]
    fn test_load_templates_none_falls_back_to_embedded() {
        // With None, it tries "web/templates" which likely doesn't exist in test CWD
        let result = load_templates(None);
        assert!(result.is_ok());
    }
}
