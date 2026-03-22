use anyhow::Result;
use console::style;

use crate::app;
use crate::cli::ConfigCommands;

pub fn run(cmd: ConfigCommands) -> Result<()> {
    match cmd {
        ConfigCommands::Show => show(),
        ConfigCommands::Edit => edit(),
    }
}

fn show() -> Result<()> {
    let path = app::config_path();

    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        // Redact auth_token value to avoid leaking secrets
        let redacted = contents
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("auth_token") && trimmed.contains('=') {
                    let prefix =
                        &line[..line.find('=').expect("line contains '=' (checked above)") + 1];
                    format!("{prefix} \"[REDACTED]\"")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!(
            "{} {}",
            style("Config:").bold(),
            style(path.display()).dim()
        );
        println!();
        println!("{redacted}");
    } else {
        println!(
            "{} No config file found at {}",
            style("INFO").dim(),
            style(path.display()).cyan()
        );
        println!();
        println!("{}", style("Default configuration:").bold());
        let config = app::AppConfig::default();
        println!(
            "{}",
            toml::to_string_pretty(&config).unwrap_or_else(|_| "Failed to serialize".into())
        );
    }

    Ok(())
}

fn edit() -> Result<()> {
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
        Ok(_) => println!("{} Config is valid.", style("OK").bold().green()),
        Err(e) => {
            println!(
                "{} Config has errors: {e}",
                style("WARNING").bold().yellow()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::app;

    #[test]
    fn test_default_config_serializes_to_valid_toml() {
        let config = app::AppConfig::default();
        let toml_str =
            toml::to_string_pretty(&config).expect("default config should serialize to TOML");
        assert!(!toml_str.is_empty());
        // Verify it can be parsed back
        let _: app::AppConfig = toml::from_str(&toml_str).expect("serialized TOML should parse");
    }

    #[test]
    fn test_config_path_is_in_voom_dir() {
        let path = app::config_path();
        let dir = app::voom_config_dir();
        assert_eq!(path.parent().unwrap(), dir);
    }
}
