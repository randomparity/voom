//! Server startup and configuration.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::DefaultBodyLimit;
use voom_domain::storage::StorageTrait;

use crate::router::build_router;
use crate::state::AppState;

/// Configuration for the web server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub template_dir: Option<String>,
    pub auth_token: Option<String>,
}

/// Start the web server.
pub async fn start_server(config: ServerConfig, store: Arc<dyn StorageTrait>) -> Result<()> {
    let templates = load_templates(config.template_dir.as_deref())?;
    let state = AppState::new(store, templates, config.auth_token);
    let router = build_router(state).layer(DefaultBodyLimit::max(2 * 1024 * 1024)); // 2 MiB

    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .context("Invalid bind address")?;

    tracing::info!("Web server listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .context("Failed to bind address")?;

    axum::serve(listener, router)
        .await
        .context("Server error")?;

    Ok(())
}

fn load_templates(template_dir: Option<&str>) -> Result<tera::Tera> {
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
            Ok(embedded_templates())
        }
    }
}

/// Embedded templates — public for integration tests.
#[must_use] 
pub fn embedded_templates_for_test() -> tera::Tera {
    embedded_templates()
}

/// Embedded templates as fallback when web/templates/ doesn't exist on disk.
fn embedded_templates() -> tera::Tera {
    let mut tera = tera::Tera::default();

    tera.add_raw_template("base.html", include_str!("../templates/base.html"))
        .expect("Failed to add base template");
    tera.add_raw_template(
        "dashboard.html",
        include_str!("../templates/dashboard.html"),
    )
    .expect("Failed to add dashboard template");
    tera.add_raw_template("library.html", include_str!("../templates/library.html"))
        .expect("Failed to add library template");
    tera.add_raw_template(
        "file_detail.html",
        include_str!("../templates/file_detail.html"),
    )
    .expect("Failed to add file_detail template");
    tera.add_raw_template("policies.html", include_str!("../templates/policies.html"))
        .expect("Failed to add policies template");
    tera.add_raw_template(
        "policy_editor.html",
        include_str!("../templates/policy_editor.html"),
    )
    .expect("Failed to add policy_editor template");
    tera.add_raw_template("jobs.html", include_str!("../templates/jobs.html"))
        .expect("Failed to add jobs template");
    tera.add_raw_template("plugins.html", include_str!("../templates/plugins.html"))
        .expect("Failed to add plugins template");
    tera.add_raw_template("settings.html", include_str!("../templates/settings.html"))
        .expect("Failed to add settings template");

    tera
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_fields() {
        let config = ServerConfig {
            host: "127.0.0.1".into(),
            port: 8080,
            template_dir: None,
            auth_token: Some("secret".into()),
        };
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 8080);
        assert!(config.template_dir.is_none());
        assert_eq!(config.auth_token, Some("secret".to_string()));
    }

    #[test]
    fn server_config_clone() {
        let config = ServerConfig {
            host: "0.0.0.0".into(),
            port: 3000,
            template_dir: Some("/tmp/templates".into()),
            auth_token: None,
        };
        let cloned = config.clone();
        assert_eq!(cloned.host, "0.0.0.0");
        assert_eq!(cloned.port, 3000);
        assert_eq!(cloned.template_dir, Some("/tmp/templates".to_string()));
        assert!(cloned.auth_token.is_none());
    }

    #[test]
    fn embedded_templates_contains_all_expected_templates() {
        let tera = embedded_templates();
        let names: Vec<&str> = tera.get_template_names().collect();
        let expected = [
            "base.html",
            "dashboard.html",
            "library.html",
            "file_detail.html",
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
    fn embedded_templates_for_test_returns_same_templates() {
        let tera = embedded_templates_for_test();
        let names: Vec<&str> = tera.get_template_names().collect();
        assert!(names.contains(&"dashboard.html"));
        assert!(names.contains(&"base.html"));
    }

    #[test]
    fn load_templates_falls_back_to_embedded_when_dir_missing() {
        // Using a non-existent directory should fall back to embedded
        let result = load_templates(Some("/nonexistent/path/that/does/not/exist"));
        assert!(result.is_ok());
        let tera = result.unwrap();
        let names: Vec<&str> = tera.get_template_names().collect();
        assert!(names.contains(&"dashboard.html"));
    }

    #[test]
    fn load_templates_none_falls_back_to_embedded() {
        // With None, it tries "web/templates" which likely doesn't exist in test CWD
        let result = load_templates(None);
        assert!(result.is_ok());
    }
}
