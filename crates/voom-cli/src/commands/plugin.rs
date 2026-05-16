use anyhow::{Context, Result, bail};
use console::style;

use crate::app;
use crate::cli::{OutputFormat, PluginCommands};
use crate::config;
use crate::output;

pub fn run(cmd: PluginCommands) -> Result<()> {
    match cmd {
        PluginCommands::List { format } => list(format),
        PluginCommands::Info { name, format } => info(&name, format),
        PluginCommands::Enable { name } => enable(&name),
        PluginCommands::Disable { name } => disable(&name),
        PluginCommands::Install { path } => install(&path),
        PluginCommands::Stats {
            plugin,
            since,
            top,
            format,
        } => crate::commands::plugin_stats::run(plugin, since, top, format),
    }
}

fn list(format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;
    let disabled = &config.plugins.disabled_plugins;
    let kernel = app::bootstrap_kernel(&config)?;

    let names = kernel.registry.plugin_names();
    let mut plugins: Vec<output::PluginListEntry> = Vec::new();

    for name in &names {
        let plugin_result = kernel.registry.get(name);
        if let Some(plugin) = plugin_result {
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
    if matches!(format, OutputFormat::Json) {
        output::print_json(&serde_json::json!({
            "plugins": plugins,
            "disabled_plugins": disabled_list,
        }))?;
    } else if total == 0 {
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

fn info(name: &str, format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;

    // Check if it's a known but disabled plugin
    if config.plugins.disabled_plugins.iter().any(|d| d == name)
        && config::KNOWN_PLUGIN_NAMES.contains(&name)
    {
        if matches!(format, OutputFormat::Json) {
            output::print_json(&serde_json::json!({
                "name": name,
                "status": "disabled",
            }))?;
            return Ok(());
        }
        println!("{} {}", style("Plugin:").bold(), style(name).cyan());
        println!("{} {}", style("Status:").bold(), style("disabled").yellow());
        println!(
            "\nUse {} to re-enable this plugin.",
            style(format!("voom plugin enable {name}")).cyan()
        );
        return Ok(());
    }

    let result = app::bootstrap_kernel_with_store(&config)?;
    let executor_capabilities = result.collector.snapshot();

    match result.kernel.registry.get(name) {
        Some(plugin) => {
            let capabilities: Vec<String> = plugin
                .capabilities()
                .iter()
                .map(|cap| cap.kind().to_string())
                .collect();
            if matches!(format, OutputFormat::Json) {
                output::print_json(&serde_json::json!({
                    "name": plugin.name(),
                    "version": plugin.version(),
                    "description": plugin.description(),
                    "author": plugin.author(),
                    "license": plugin.license(),
                    "homepage": plugin.homepage(),
                    "status": "enabled",
                    "capabilities": capabilities,
                }))?;
                return Ok(());
            }
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
            for cap in capabilities {
                println!("  - {cap}");
            }

            // Show executor details if available
            output::format_executor_capabilities(name, &executor_capabilities);
        }
        _ => {
            let available = result.kernel.registry.plugin_names().join(", ");
            anyhow::bail!("Plugin \"{name}\" not found. Available: {available}");
        }
    }

    Ok(())
}

fn enable(name: &str) -> Result<()> {
    set_plugin_enabled(name, true)
}

fn disable(name: &str) -> Result<()> {
    set_plugin_enabled(name, false)
}

/// Shared implementation for enable/disable: validates the plugin name, checks
/// current state, mutates the disabled list, and saves the config.
fn set_plugin_enabled(name: &str, enabled: bool) -> Result<()> {
    let mut config = config::load_config()?;
    let changed = apply_plugin_enable_change(&mut config, name, enabled)?;

    let state_style = if enabled {
        style("enabled").green()
    } else {
        style("disabled").yellow()
    };

    if !changed {
        println!(
            "Plugin \"{}\" is already {}.",
            style(name).cyan(),
            state_style
        );
        return Ok(());
    }

    config::save_config(&config)?;
    println!(
        "{} Plugin \"{}\" has been {}.",
        style("OK").bold().green(),
        style(name).cyan(),
        state_style
    );
    Ok(())
}

/// Apply an enable/disable transition to an in-memory config.
///
/// Returns `true` if the config was modified and needs to be saved. Returns
/// `false` if the plugin was already in the requested state (no-op).
///
/// Rejects:
/// - Unknown plugin names (not in `KNOWN_PLUGIN_NAMES`).
/// - Attempts to disable a plugin in `REQUIRED_PLUGIN_NAMES`. Required
///   plugins are gated at the CLI boundary so users never reach the cryptic
///   bootstrap-time validation error from `AppConfig::validate`.
fn apply_plugin_enable_change(
    config: &mut config::AppConfig,
    name: &str,
    enabled: bool,
) -> Result<bool> {
    if !config::KNOWN_PLUGIN_NAMES.contains(&name) {
        let known = config::KNOWN_PLUGIN_NAMES.join(", ");
        bail!("Unknown plugin \"{name}\". Known plugins: {known}");
    }
    if !enabled && config::REQUIRED_PLUGIN_NAMES.contains(&name) {
        let required = config::REQUIRED_PLUGIN_NAMES.join(", ");
        bail!(
            "Cannot disable required plugin \"{name}\". \
             Required plugins (the scan/process commands fail to bootstrap without them): {required}."
        );
    }

    let is_disabled = config.plugins.disabled_plugins.iter().any(|d| d == name);
    if enabled != is_disabled {
        return Ok(false);
    }

    if enabled {
        config.plugins.disabled_plugins.retain(|d| d != name);
    } else {
        config.plugins.disabled_plugins.push(name.to_string());
    }
    Ok(true)
}

fn install(path: &std::path::Path) -> Result<()> {
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

    std::fs::copy(path, &target_wasm).with_context(|| {
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
        let result = install(std::path::Path::new("/nonexistent/plugin.wasm"));
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

        let result = install(&file);
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

        let result = install(&file);
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

    #[test]
    fn cannot_disable_required_plugins_via_cli() {
        // For every name in REQUIRED_PLUGIN_NAMES, attempting to disable it
        // must fail with an actionable error message naming the plugin.
        // This guards the CLI boundary so users never reach the cryptic
        // bootstrap-time config-validation failure.
        for name in config::REQUIRED_PLUGIN_NAMES {
            let mut cfg = config::AppConfig::default();
            let err = apply_plugin_enable_change(&mut cfg, name, false)
                .expect_err(&format!("disable of required plugin {name} must fail"));
            let msg = err.to_string();
            assert!(
                msg.contains("Cannot disable required plugin"),
                "error must explain why disable was rejected; got: {msg}"
            );
            assert!(
                msg.contains(name),
                "error must name the rejected plugin {name}; got: {msg}"
            );
        }
    }

    #[test]
    fn enabling_required_plugin_still_succeeds() {
        // Idempotent: enabling a required plugin from its default state
        // (enabled) is a no-op success. Confirms the rejection gate is
        // bounded to the disable direction.
        for name in config::REQUIRED_PLUGIN_NAMES {
            let mut cfg = config::AppConfig::default();
            let changed = apply_plugin_enable_change(&mut cfg, name, true)
                .unwrap_or_else(|e| panic!("enable of required plugin {name} should succeed: {e}"));
            assert!(
                !changed,
                "enabling an already-enabled required plugin must be a no-op"
            );
        }
    }
}
