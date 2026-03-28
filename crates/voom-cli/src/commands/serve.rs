use anyhow::Result;
use console::style;
use tokio_util::sync::CancellationToken;
use voom_web_server::server::{start_server, ServerConfig};

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs, token: CancellationToken) -> Result<()> {
    let config = crate::config::load_config()?;
    let (kernel, store) = crate::app::bootstrap_kernel_with_store(&config)?;

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
                    p.capabilities()
                        .iter()
                        .map(|c| c.kind().to_string())
                        .collect(),
                )
            })
        })
        .collect();

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
