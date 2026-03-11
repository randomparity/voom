//! Server startup and configuration.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
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
    let router = build_router(state);

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
