use std::path::PathBuf;

use anyhow::{bail, Context, Result};
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
    let disabled = &config.plugins.disabled_plugins;
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

    // Collect disabled plugins that are known but not loaded
    let mut disabled_list: Vec<String> = Vec::new();
    for d in disabled {
        if app::KNOWN_PLUGIN_NAMES.contains(&d.as_str()) && !names.contains(d) {
            disabled_list.push(d.clone());
        }
    }

    let total = plugins.len() + disabled_list.len();
    if total == 0 {
        println!("{}", "No plugins registered.".dimmed());
    } else {
        println!(
            "{} registered plugins{}:\n",
            total.to_string().bold(),
            if disabled_list.is_empty() {
                String::new()
            } else {
                format!(" ({} disabled)", disabled_list.len())
            }
        );
        output::format_plugin_list(&plugins);

        if !disabled_list.is_empty() {
            println!("\n{}", "Disabled plugins:".dimmed());
            for name in &disabled_list {
                println!("  {} {}", "-".dimmed(), name.dimmed());
            }
        }
    }

    Ok(())
}

async fn info(name: String) -> Result<()> {
    let config = app::load_config()?;

    // Check if it's a known but disabled plugin
    if config.plugins.disabled_plugins.contains(&name)
        && app::KNOWN_PLUGIN_NAMES.contains(&name.as_str())
    {
        println!("{} {}", "Plugin:".bold(), name.cyan());
        println!("{} {}", "Status:".bold(), "disabled".yellow());
        println!(
            "\nUse {} to re-enable this plugin.",
            format!("voom plugin enable {name}").cyan()
        );
        return Ok(());
    }

    let kernel = app::bootstrap_kernel(&config)?;

    match kernel.registry.get(&name) {
        Some(plugin) => {
            println!("{} {}", "Plugin:".bold(), plugin.name().cyan());
            println!("{} {}", "Version:".bold(), plugin.version());
            println!("{} {}", "Status:".bold(), "enabled".green());
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
    // Validate that this is a known plugin name
    if !app::KNOWN_PLUGIN_NAMES.contains(&name.as_str()) {
        println!("{} Unknown plugin \"{}\".", "ERROR".bold().red(), name);
        println!("\nKnown plugins:");
        for n in app::KNOWN_PLUGIN_NAMES {
            println!("  - {n}");
        }
        std::process::exit(1);
    }

    let mut config = app::load_config()?;

    if !config.plugins.disabled_plugins.contains(&name) {
        println!(
            "Plugin \"{}\" is already {}.",
            name.cyan(),
            "enabled".green()
        );
        return Ok(());
    }

    config.plugins.disabled_plugins.retain(|d| d != &name);
    app::save_config(&config)?;

    println!(
        "{} Plugin \"{}\" has been {}.",
        "OK".bold().green(),
        name.cyan(),
        "enabled".green()
    );
    Ok(())
}

async fn disable(name: String) -> Result<()> {
    // Validate that this is a known plugin name
    if !app::KNOWN_PLUGIN_NAMES.contains(&name.as_str()) {
        println!("{} Unknown plugin \"{}\".", "ERROR".bold().red(), name);
        println!("\nKnown plugins:");
        for n in app::KNOWN_PLUGIN_NAMES {
            println!("  - {n}");
        }
        std::process::exit(1);
    }

    let mut config = app::load_config()?;

    if config.plugins.disabled_plugins.contains(&name) {
        println!(
            "Plugin \"{}\" is already {}.",
            name.cyan(),
            "disabled".yellow()
        );
        return Ok(());
    }

    config.plugins.disabled_plugins.push(name.clone());
    app::save_config(&config)?;

    println!(
        "{} Plugin \"{}\" has been {}.",
        "OK".bold().green(),
        name.cyan(),
        "disabled".yellow()
    );
    Ok(())
}

async fn install(path: PathBuf) -> Result<()> {
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
    let config = app::load_config()?;
    let target_dir = config
        .plugins
        .wasm_dir
        .unwrap_or_else(|| app::voom_config_dir().join("plugins").join("wasm"));

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
        "OK".bold().green(),
        manifest.name.cyan(),
        manifest.version,
        target_dir.display().to_string().dimmed()
    );

    Ok(())
}
