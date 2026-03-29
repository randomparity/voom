use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use console::style;

use crate::app;
use crate::cli::PluginCommands;
use crate::config;
use crate::output;

pub fn run(cmd: PluginCommands) -> Result<()> {
    match cmd {
        PluginCommands::List => list(),
        PluginCommands::Info { name } => info(name),
        PluginCommands::Enable { name } => enable(name),
        PluginCommands::Disable { name } => disable(name),
        PluginCommands::Install { path } => install(path),
    }
}

fn list() -> Result<()> {
    let config = config::load_config()?;
    let disabled = &config.plugins.disabled_plugins;
    let kernel = app::bootstrap_kernel(&config)?;

    let names = kernel.registry.plugin_names();
    let mut plugins: Vec<output::PluginListEntry> = Vec::new();

    for name in &names {
        if let Some(plugin) = kernel.registry.get(name) {
            let caps: Vec<String> = plugin
                .capabilities()
                .iter()
                .map(|c| c.kind().to_string())
                .collect();
            plugins.push(output::PluginListEntry {
                name: name.clone(),
                version: plugin.version().to_string(),
                description: plugin.description().to_string(),
                capabilities: caps,
            });
        }
    }

    // Collect disabled plugins that are known but not loaded
    let mut disabled_list: Vec<String> = Vec::new();
    for d in disabled {
        if config::KNOWN_PLUGIN_NAMES.contains(&d.as_str()) && !names.contains(d) {
            disabled_list.push(d.clone());
        }
    }

    let total = plugins.len() + disabled_list.len();
    if total == 0 {
        println!("{}", style("No plugins registered.").dim());
    } else {
        println!(
            "{} registered plugins{}:\n",
            style(total).bold(),
            if disabled_list.is_empty() {
                String::new()
            } else {
                format!(" ({} disabled)", disabled_list.len())
            }
        );
        output::format_plugin_list(&plugins);

        if !disabled_list.is_empty() {
            println!("\n{}", style("Disabled plugins:").dim());
            for name in &disabled_list {
                println!("  {} {}", style("-").dim(), style(name).dim());
            }
        }
    }

    Ok(())
}

fn info(name: String) -> Result<()> {
    let config = config::load_config()?;

    // Check if it's a known but disabled plugin
    if config.plugins.disabled_plugins.contains(&name)
        && config::KNOWN_PLUGIN_NAMES.contains(&name.as_str())
    {
        println!("{} {}", style("Plugin:").bold(), style(&name).cyan());
        println!("{} {}", style("Status:").bold(), style("disabled").yellow());
        println!(
            "\nUse {} to re-enable this plugin.",
            style(format!("voom plugin enable {name}")).cyan()
        );
        return Ok(());
    }

    let result = app::bootstrap_kernel_with_store(&config)?;
    let capabilities = result.collector.snapshot();

    match result.kernel.registry.get(&name) {
        Some(plugin) => {
            println!(
                "{} {}",
                style("Plugin:").bold(),
                style(plugin.name()).cyan()
            );
            println!("{} {}", style("Version:").bold(), plugin.version());
            if !plugin.description().is_empty() {
                println!("{} {}", style("Description:").bold(), plugin.description());
            }
            if !plugin.author().is_empty() {
                println!("{} {}", style("Author:").bold(), plugin.author());
            }
            if !plugin.license().is_empty() {
                println!("{} {}", style("License:").bold(), plugin.license());
            }
            if !plugin.homepage().is_empty() {
                println!("{} {}", style("Homepage:").bold(), plugin.homepage());
            }
            println!("{} {}", style("Status:").bold(), style("enabled").green());
            println!("{}", style("Capabilities:").bold());
            for cap in plugin.capabilities() {
                println!("  - {}", cap.kind());
            }

            // Show executor details if available
            if let Some(caps) = capabilities.executor_capabilities(&name) {
                if !caps.hw_accels.is_empty() {
                    let best = capabilities.best_hwaccel();
                    println!("{}", style("Hardware Acceleration:").bold());
                    println!(
                        "  {} {} ({})",
                        style("Backend:").bold(),
                        style(best).green(),
                        caps.hw_accels.join(", ")
                    );
                }
                if !caps.codecs.decoders.is_empty() || !caps.codecs.encoders.is_empty() {
                    println!("{}", style("Codecs:").bold());
                    if !caps.codecs.decoders.is_empty() {
                        println!(
                            "  {} ({}): {}",
                            style("Decoders").bold(),
                            caps.codecs.decoders.len(),
                            caps.codecs.decoders.join(", ")
                        );
                    }
                    if !caps.codecs.encoders.is_empty() {
                        println!(
                            "  {} ({}): {}",
                            style("Encoders").bold(),
                            caps.codecs.encoders.len(),
                            caps.codecs.encoders.join(", ")
                        );
                    }
                }
                if !caps.formats.is_empty() {
                    println!(
                        "{} ({}): {}",
                        style("Formats:").bold(),
                        caps.formats.len(),
                        caps.formats.join(", ")
                    );
                }
            }
        }
        None => {
            let available = result.kernel.registry.plugin_names().join(", ");
            anyhow::bail!("Plugin \"{name}\" not found. Available: {available}");
        }
    }

    Ok(())
}

fn enable(name: String) -> Result<()> {
    set_plugin_enabled(name, true)
}

fn disable(name: String) -> Result<()> {
    set_plugin_enabled(name, false)
}

/// Shared implementation for enable/disable: validates the plugin name, checks
/// current state, mutates the disabled list, and saves the config.
fn set_plugin_enabled(name: String, enabled: bool) -> Result<()> {
    if !config::KNOWN_PLUGIN_NAMES.contains(&name.as_str()) {
        let known = config::KNOWN_PLUGIN_NAMES.join(", ");
        bail!("Unknown plugin \"{name}\". Known plugins: {known}");
    }

    let mut config = config::load_config()?;
    let is_disabled = config.plugins.disabled_plugins.contains(&name);

    if enabled && !is_disabled {
        println!(
            "Plugin \"{}\" is already {}.",
            style(&name).cyan(),
            style("enabled").green()
        );
        return Ok(());
    }

    if !enabled && is_disabled {
        println!(
            "Plugin \"{}\" is already {}.",
            style(&name).cyan(),
            style("disabled").yellow()
        );
        return Ok(());
    }

    if enabled {
        config.plugins.disabled_plugins.retain(|d| d != &name);
    } else {
        config.plugins.disabled_plugins.push(name.clone());
    }
    config::save_config(&config)?;

    if enabled {
        println!(
            "{} Plugin \"{}\" has been {}.",
            style("OK").bold().green(),
            style(&name).cyan(),
            style("enabled").green()
        );
    } else {
        println!(
            "{} Plugin \"{}\" has been {}.",
            style("OK").bold().green(),
            style(&name).cyan(),
            style("disabled").yellow()
        );
    }
    Ok(())
}

fn install(path: PathBuf) -> Result<()> {
    // 1. Check the path exists and has .wasm extension
    if !path.exists() {
        bail!("File not found: {}", path.display());
    }

    match path.extension().and_then(|e| e.to_str()) {
        Some("wasm") => {}
        _ => bail!("Expected a .wasm file, got: {}", path.display()),
    }

    // 2. Look for a sibling .toml manifest file
    let manifest_path = path.with_extension("toml");
    if !manifest_path.exists() {
        bail!(
            "Manifest file not found: {}\n\
             A .toml manifest file must exist alongside the .wasm file.",
            manifest_path.display()
        );
    }

    // 3. Read and validate the manifest
    let manifest_str = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;

    let manifest: voom_kernel::manifest::PluginManifest = toml::from_str(&manifest_str)
        .with_context(|| format!("Failed to parse manifest: {}", manifest_path.display()))?;

    if let Err(errors) = manifest.validate() {
        bail!(
            "Invalid plugin manifest:\n{}",
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // 4. Determine target directory
    let cfg = config::load_config()?;
    let target_dir = cfg
        .plugins
        .wasm_dir
        .unwrap_or_else(|| config::voom_config_dir().join("plugins").join("wasm"));

    // 5. Create target directory if needed
    std::fs::create_dir_all(&target_dir).with_context(|| {
        format!(
            "Failed to create plugin directory: {}",
            target_dir.display()
        )
    })?;

    // 6. Copy .wasm and .toml files
    let wasm_filename = path.file_name().context("Invalid .wasm filename")?;
    let manifest_filename = manifest_path
        .file_name()
        .context("Invalid .toml filename")?;

    let target_wasm = target_dir.join(wasm_filename);
    let target_manifest = target_dir.join(manifest_filename);

    std::fs::copy(&path, &target_wasm).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            path.display(),
            target_wasm.display()
        )
    })?;

    std::fs::copy(&manifest_path, &target_manifest).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            manifest_path.display(),
            target_manifest.display()
        )
    })?;

    // 7. Print success
    println!(
        "{} Installed plugin \"{}\" v{} to {}",
        style("OK").bold().green(),
        style(&manifest.name).cyan(),
        manifest.version,
        style(target_dir.display()).dim()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_nonexistent_file_fails() {
        let result = install(PathBuf::from("/nonexistent/plugin.wasm"));
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("not found"),
            "should report file not found"
        );
    }

    #[test]
    fn test_install_non_wasm_extension_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugin.txt");
        std::fs::write(&file, "not a wasm file").unwrap();

        let result = install(file);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains(".wasm"),
            "should mention .wasm requirement"
        );
    }

    #[test]
    fn test_install_wasm_without_manifest_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugin.wasm");
        std::fs::write(&file, b"\0asm").unwrap();

        let result = install(file);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Manifest"),
            "should report missing manifest"
        );
    }

    #[test]
    fn test_known_plugins_enable_disable_validation() {
        // Verify that enable/disable check against KNOWN_PLUGIN_NAMES
        assert!(config::KNOWN_PLUGIN_NAMES.contains(&"sqlite-store"));
        assert!(!config::KNOWN_PLUGIN_NAMES.contains(&"nonexistent-plugin"));
    }
}
