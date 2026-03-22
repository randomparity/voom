use anyhow::Result;
use console::style;
use tokio_util::sync::CancellationToken;
use voom_web_server::server::{start_server, ServerConfig};

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs, token: CancellationToken) -> Result<()> {
    let config = crate::config::load_config()?;
    let (kernel, store) = crate::app::bootstrap_kernel_with_store(&config)?;

    // Snapshot plugin info from the kernel registry
    let plugin_info: Vec<voom_web_server::api::plugins::PluginInfo> = kernel
        .registry
        .plugin_names()
        .iter()
        .filter_map(|name| {
            kernel
                .registry
                .get(name)
                .map(|p| voom_web_server::api::plugins::PluginInfo {
                    name: p.name().to_string(),
                    version: p.version().to_string(),
                    capabilities: p
                        .capabilities()
                        .iter()
                        .map(|c| c.kind().to_string())
                        .collect(),
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

    let server_config = ServerConfig {
        host: args.host,
        port: args.port,
        template_dir: None,
        auth_token: config.auth_token,
        plugin_info,
    };

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
        let server_config = ServerConfig {
            host: args.host.clone(),
            port: args.port,
            template_dir: None,
            auth_token: None,
            plugin_info: vec![],
        };
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
        let server_config = ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
            template_dir: None,
            auth_token: config.auth_token.clone(),
            plugin_info: vec![],
        };
        assert_eq!(server_config.auth_token.as_deref(), Some("secret"));
        assert_eq!(server_config.port, 3000);
    }
}
