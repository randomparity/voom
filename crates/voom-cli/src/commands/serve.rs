use std::sync::Arc;

use anyhow::Result;
use owo_colors::OwoColorize;
use voom_domain::storage::StorageTrait;
use voom_web_server::server::{ServerConfig, start_server};

use crate::cli::ServeArgs;

pub async fn run(args: ServeArgs) -> Result<()> {
    let config = crate::app::load_config()?;
    let store: Arc<dyn StorageTrait> = crate::app::open_store(&config)?;

    println!(
        "{} Starting VOOM web server on {}:{}",
        "●".bold().green(),
        args.host.cyan(),
        args.port.to_string().cyan()
    );
    println!(
        "  {} http://{}:{}",
        "→".bold(),
        args.host,
        args.port
    );

    let server_config = ServerConfig {
        host: args.host,
        port: args.port,
        template_dir: None,
    };

    start_server(server_config, store).await?;

    Ok(())
}
