use std::sync::Arc;

use anyhow::Result;
use owo_colors::OwoColorize;
use tokio_util::sync::CancellationToken;
use voom_domain::storage::StorageTrait;
use voom_web_server::server::{start_server, ServerConfig};

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs, token: CancellationToken) -> Result<()> {
    let config = crate::app::load_config()?;
    let store: Arc<dyn StorageTrait> = crate::app::open_store(&config)?;

    println!(
        "{} Starting VOOM web server on {}:{}",
        "●".bold().green(),
        args.host.cyan(),
        args.port.to_string().cyan()
    );
    println!("  {} http://{}:{}", "→".bold(), args.host, args.port);

    let server_config = ServerConfig {
        host: args.host,
        port: args.port,
        template_dir: None,
        auth_token: config.auth_token,
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
    fn server_config_from_default_args() {
        let args = ServeArgs {
            port: 8080,
            host: "127.0.0.1".to_string(),
        };
        let server_config = ServerConfig {
            host: args.host.clone(),
            port: args.port,
            template_dir: None,
            auth_token: None,
        };
        assert_eq!(server_config.port, 8080);
        assert_eq!(server_config.host, "127.0.0.1");
        assert!(server_config.auth_token.is_none());
    }

    #[test]
    fn server_config_with_auth_token() {
        let config = crate::app::AppConfig {
            data_dir: std::path::PathBuf::from("/tmp"),
            plugins: crate::app::PluginsConfig::default(),
            auth_token: Some("secret".to_string()),
            plugin: std::collections::HashMap::new(),
        };
        let server_config = ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 3000,
            template_dir: None,
            auth_token: config.auth_token.clone(),
        };
        assert_eq!(server_config.auth_token.as_deref(), Some("secret"));
        assert_eq!(server_config.port, 3000);
    }
}
