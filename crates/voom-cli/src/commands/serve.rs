use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use console::style;
use tokio_util::sync::CancellationToken;
use voom_web_server::server::{start_server, ServerConfig};
use voom_web_server::state::{SseEvent, SSE_CHANNEL_CAPACITY};

use crate::cli::ServeArgs;

/// Priority for the web-sse-bridge plugin. In the VOOM event bus, lower
/// priority numbers dispatch first; 200 is the highest registered value
/// in the system, so the bridge runs last and observes events only after
/// job-manager (20) and sqlite-store (100) have already logged and
/// persisted them.
const PRIORITY_WEB_SSE_BRIDGE: i32 = 200;

pub async fn run(args: ServeArgs, token: CancellationToken) -> Result<()> {
    let config = crate::config::load_config()?;
    let crate::app::BootstrapResult {
        mut kernel, store, ..
    } = crate::app::bootstrap_kernel_with_store(&config)?;

    // Create the SSE broadcast channel and register a kernel-side bridge plugin
    // that forwards relevant bus events into it. The same sender is then handed
    // to the web server so connected clients receive the broadcasts.
    let (sse_tx, _) = tokio::sync::broadcast::channel::<SseEvent>(SSE_CHANNEL_CAPACITY);
    let bridge = voom_web_sse_bridge::WebSseBridgePlugin::new(sse_tx.clone());
    let bridge_ctx =
        voom_kernel::PluginContext::new(serde_json::json!({}), config.data_dir.clone());
    kernel
        .init_and_register(Arc::new(bridge), PRIORITY_WEB_SSE_BRIDGE, &bridge_ctx)
        .context("Failed to register web-sse-bridge plugin")?;

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
        .and_then(|t| t.clone().try_into().ok())
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
            let prune_every = (86_400 / health_interval).max(1); // ~once per day
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
                    () = health_token.cancelled() => break,
                }
            }
        });
    }

    // ── Retention task ────────────────────────────────────────────
    let retention_runner = Arc::new(crate::retention::RetentionRunner::new(
        store.clone(),
        config.retention.clone(),
        Some(kernel.clone()),
    ));
    let retention_interval_secs =
        u64::from(config.retention.schedule_interval_minutes).saturating_mul(60);

    if retention_interval_secs > 0 && !retention_runner.is_fully_disabled() {
        let retention_token = token.clone();
        let runner = retention_runner.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(retention_interval_secs));
            interval.tick().await; // skip immediate first tick
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let r = runner.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            r.run_once(voom_domain::events::RetentionTrigger::Scheduled);
                        }).await;
                    }
                    () = retention_token.cancelled() => break,
                }
            }
        });
    } else {
        tracing::info!("retention task disabled (interval=0 or all tables disabled)");
    }

    println!(
        "{} Starting VOOM web server on {}:{}",
        style("●").bold().green(),
        style(&args.host).cyan(),
        style(args.port).cyan()
    );
    println!("  {} http://{}:{}", style("→").bold(), args.host, args.port);

    let mut server_config = ServerConfig::new(args.host, args.port, sse_tx);
    server_config.auth_token = config.auth_token;
    server_config.plugin_info = plugin_info;
    server_config.data_dir = Some(config.data_dir.clone());

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
        use tokio::sync::broadcast;
        use voom_web_server::state::SseEvent;
        let args = ServeArgs {
            port: 8080,
            host: "127.0.0.1".to_string(),
        };
        let (sse_tx, _) = broadcast::channel::<SseEvent>(1);
        let server_config = ServerConfig::new(args.host.clone(), args.port, sse_tx);
        assert_eq!(server_config.port, 8080);
        assert_eq!(server_config.host, "127.0.0.1");
        assert!(server_config.auth_token.is_none());
    }

    #[test]
    fn retention_runner_is_disabled_when_all_zero() {
        use std::sync::Arc;
        use voom_domain::test_support::InMemoryStore;

        let cfg = crate::config::RetentionConfig {
            schedule_interval_minutes: 60,
            run_after_cli: true,
            jobs: crate::config::TableRetention {
                keep_for_days: Some(0),
                keep_last: Some(0),
            },
            event_log: crate::config::TableRetention {
                keep_for_days: Some(0),
                keep_last: Some(0),
            },
            file_transitions: crate::config::TableRetention {
                keep_for_days: Some(0),
                keep_last: Some(0),
            },
        };

        let store: Arc<dyn voom_domain::storage::StorageTrait> = Arc::new(InMemoryStore::new());
        let runner = crate::retention::RetentionRunner::new(store, cfg, None);
        assert!(runner.is_fully_disabled());
    }

    #[test]
    fn test_server_config_with_auth_token() {
        use tokio::sync::broadcast;
        use voom_web_server::state::SseEvent;
        let config = crate::config::AppConfig {
            auth_token: Some("secret".to_string()),
            ..Default::default()
        };
        let (sse_tx, _) = broadcast::channel::<SseEvent>(1);
        let mut server_config = ServerConfig::new("0.0.0.0".to_string(), 3000, sse_tx);
        server_config.auth_token = config.auth_token.clone();
        assert_eq!(server_config.auth_token.as_deref(), Some("secret"));
        assert_eq!(server_config.port, 3000);
    }
}
