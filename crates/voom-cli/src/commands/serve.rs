use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use console::style;
use tokio_util::sync::CancellationToken;
use voom_web_server::server::{start_server, ServerConfig};

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs, token: CancellationToken) -> Result<()> {
    let config = crate::config::load_config()?;
    let crate::app::BootstrapResult { kernel, store, .. } =
        crate::app::bootstrap_kernel_with_store(&config)?;

    // Snapshot plugin info from the kernel registry
    let plugin_info: Vec<voom_web_server::api::plugins::PluginInfoResponse> = kernel
        .registry
        .plugin_names()
        .iter()
        .filter_map(|name| {
            kernel.registry.get(name).map(|p| {
                voom_web_server::api::plugins::PluginInfoResponse::new(
                    p.name().to_string(),
                    p.version().to_string(),
                    p.description().to_string(),
                    p.author().to_string(),
                    p.license().to_string(),
                    p.homepage().to_string(),
                    p.capabilities()
                        .iter()
                        .map(|c| c.kind().to_string())
                        .collect(),
                )
            })
        })
        .collect();

    // Parse health-checker config once (single source of truth for defaults).
    let health_config: voom_health_checker::HealthCheckerConfig = config
        .plugin
        .get("health-checker")
        .and_then(|t| serde_json::to_value(t).ok())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let health_interval = health_config.interval_secs;
    let retention_days = i64::from(health_config.retention_days);

    // Wrap kernel in Arc so the health-check background task can share it
    // with any future consumers. The kernel is not used after this point
    // by the main task (plugin_info snapshot was taken above).
    let kernel = Arc::new(kernel);

    if health_interval > 0 {
        let health_token = token.clone();
        let data_dir = config.data_dir.clone();
        let health_kernel = kernel.clone();
        let health_store = store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(health_interval));
            interval.tick().await; // skip immediate first tick (init already ran checks)
            let prune_every = 86_400 / health_interval; // ~once per day
            let mut tick_count: u64 = 0;
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        tick_count += 1;

                        // Prune old records ~once per day
                        if tick_count % prune_every == 0 {
                            let prune_store = health_store.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                let cutoff = chrono::Utc::now()
                                    - chrono::Duration::days(retention_days);
                                if let Err(e) = prune_store.prune_health_checks(cutoff) {
                                    tracing::warn!(error = %e, "failed to prune health checks");
                                }
                            }).await;
                        }

                        // Run health checks and dispatch events
                        let k = health_kernel.clone();
                        let d = data_dir.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let checker = voom_health_checker::HealthCheckerPlugin::new();
                            let events = checker.run_checks(&d);
                            for event in events {
                                k.dispatch(event);
                            }
                        }).await;
                    }
                    _ = health_token.cancelled() => break,
                }
            }
        });
    }

    println!(
        "{} Starting VOOM web server on {}:{}",
        style("●").bold().green(),
        style(&args.host).cyan(),
        style(args.port).cyan()
    );
    println!("  {} http://{}:{}", style("→").bold(), args.host, args.port);

    let mut server_config = ServerConfig::new(args.host, args.port);
    server_config.auth_token = config.auth_token;
    server_config.plugin_info = plugin_info;

    let shutdown = async move { token.cancelled().await };
    start_server(server_config, store, shutdown).await?;

    println!("Server stopped.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ServeArgs;

    #[test]
    fn test_server_config_from_default_args() {
        let args = ServeArgs {
            port: 8080,
            host: "127.0.0.1".to_string(),
        };
        let server_config = ServerConfig::new(args.host.clone(), args.port);
        assert_eq!(server_config.port, 8080);
        assert_eq!(server_config.host, "127.0.0.1");
        assert!(server_config.auth_token.is_none());
    }

    #[test]
    fn test_server_config_with_auth_token() {
        let config = crate::config::AppConfig {
            data_dir: std::path::PathBuf::from("/tmp"),
            plugins: crate::config::PluginsConfig::default(),
            auth_token: Some("secret".to_string()),
            plugin: std::collections::HashMap::new(),
        };
        let mut server_config = ServerConfig::new("0.0.0.0".to_string(), 3000);
        server_config.auth_token = config.auth_token.clone();
        assert_eq!(server_config.auth_token.as_deref(), Some("secret"));
        assert_eq!(server_config.port, 3000);
    }
}
