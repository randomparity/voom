use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::PluginCommands;
use crate::output;

pub async fn run(cmd: PluginCommands) -> Result<()> {
    match cmd {
        PluginCommands::List => list().await,
        PluginCommands::Info { name } => info(name).await,
        PluginCommands::Enable { name } => enable(name).await,
        PluginCommands::Disable { name } => disable(name).await,
        PluginCommands::Install { path } => install(path).await,
    }
}

async fn list() -> Result<()> {
    let config = app::load_config()?;
    let kernel = app::bootstrap_kernel(&config)?;

    let names = kernel.registry.plugin_names();
    let mut plugins: Vec<(String, String, Vec<String>)> = Vec::new();

    for name in &names {
        if let Some(plugin) = kernel.registry.get(name) {
            let caps: Vec<String> = plugin
                .capabilities()
                .iter()
                .map(|c| c.kind().to_string())
                .collect();
            plugins.push((name.clone(), plugin.version().to_string(), caps));
        }
    }

    if plugins.is_empty() {
        println!("{}", "No plugins registered.".dimmed());
    } else {
        println!("{} registered plugins:\n", plugins.len().to_string().bold());
        output::format_plugin_list(&plugins);
    }

    Ok(())
}

async fn info(name: String) -> Result<()> {
    let config = app::load_config()?;
    let kernel = app::bootstrap_kernel(&config)?;

    match kernel.registry.get(&name) {
        Some(plugin) => {
            println!("{} {}", "Plugin:".bold(), plugin.name().cyan());
            println!("{} {}", "Version:".bold(), plugin.version());
            println!("{}", "Capabilities:".bold());
            for cap in plugin.capabilities() {
                println!("  - {}", cap.kind());
            }
        }
        None => {
            println!("{} Plugin \"{}\" not found.", "ERROR".bold().red(), name);
            println!("\nAvailable plugins:");
            for n in kernel.registry.plugin_names() {
                println!("  - {n}");
            }
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn enable(name: String) -> Result<()> {
    println!(
        "{} Plugin enable/disable is not yet implemented.",
        "TODO".bold().yellow()
    );
    println!("Plugin: {name}");
    Ok(())
}

async fn disable(name: String) -> Result<()> {
    println!(
        "{} Plugin enable/disable is not yet implemented.",
        "TODO".bold().yellow()
    );
    println!("Plugin: {name}");
    Ok(())
}

async fn install(path: std::path::PathBuf) -> Result<()> {
    println!(
        "{} WASM plugin installation is not yet implemented.",
        "TODO".bold().yellow()
    );
    println!("Path: {}", path.display());
    Ok(())
}
