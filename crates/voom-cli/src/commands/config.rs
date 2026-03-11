use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::ConfigCommands;

pub async fn run(cmd: ConfigCommands) -> Result<()> {
    match cmd {
        ConfigCommands::Show => show().await,
        ConfigCommands::Edit => edit().await,
    }
}

async fn show() -> Result<()> {
    let path = app::config_path();

    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        println!(
            "{} {}",
            "Config:".bold(),
            path.display().to_string().dimmed()
        );
        println!();
        println!("{contents}");
    } else {
        println!(
            "{} No config file found at {}",
            "INFO".dimmed(),
            path.display().to_string().cyan()
        );
        println!();
        println!("{}", "Default configuration:".bold());
        let config = app::AppConfig::default();
        println!(
            "{}",
            toml::to_string_pretty(&config).unwrap_or_else(|_| "Failed to serialize".into())
        );
    }

    Ok(())
}

async fn edit() -> Result<()> {
    let path = app::config_path();

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Create default config if it doesn't exist
    if !path.exists() {
        let config = app::AppConfig::default();
        let contents = toml::to_string_pretty(&config)?;
        std::fs::write(&path, contents)?;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor).arg(&path).status()?;

    if !status.success() {
        anyhow::bail!("Editor exited with status: {status}");
    }

    // Validate the edited config
    match app::load_config() {
        Ok(_) => println!("{} Config is valid.", "OK".bold().green()),
        Err(e) => {
            println!("{} Config has errors: {e}", "WARNING".bold().yellow());
        }
    }

    Ok(())
}
