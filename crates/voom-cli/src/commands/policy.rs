use anyhow::{Context, Result};
use owo_colors::OwoColorize;

use crate::cli::PolicyCommands;

pub async fn run(cmd: PolicyCommands) -> Result<()> {
    match cmd {
        PolicyCommands::List => list().await,
        PolicyCommands::Validate { file } => validate(file).await,
        PolicyCommands::Show { file } => show(file).await,
        PolicyCommands::Format { file } => format(file).await,
    }
}

async fn list() -> Result<()> {
    // Scan standard policy directories
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("voom")
        .join("policies");

    if !config_dir.exists() {
        println!("{}", "No policies directory found.".dimmed());
        println!(
            "Create policies in: {}",
            config_dir.display().to_string().cyan()
        );
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(&config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "voom") {
            let name = path.file_stem().unwrap().to_string_lossy();
            match voom_dsl::compile(&std::fs::read_to_string(&path)?) {
                Ok(policy) => {
                    println!(
                        "  {} {} ({} phases)",
                        "OK".green(),
                        policy.name.bold(),
                        policy.phases.len()
                    );
                }
                Err(e) => {
                    println!("  {} {} — {e}", "ERR".red(), name.bold());
                }
            }
            found = true;
        }
    }

    if !found {
        println!("{}", "No .voom policy files found.".dimmed());
    }

    Ok(())
}

async fn validate(file: std::path::PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    match voom_dsl::compile(&source) {
        Ok(policy) => {
            println!(
                "{} Policy \"{}\" is valid ({} phases, {} phase order: [{}])",
                "OK".bold().green(),
                policy.name,
                policy.phases.len(),
                policy.phase_order.len(),
                policy.phase_order.join(", ")
            );
        }
        Err(e) => {
            println!("{} {e}", "ERROR".bold().red());
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn show(file: std::path::PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    let compiled = voom_dsl::compile(&source).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("{} \"{}\"", "Policy".bold(), compiled.name.cyan());
    println!();

    // Config
    println!("{}", "Config:".bold());
    println!(
        "  Audio languages: [{}]",
        compiled.config.audio_languages.join(", ")
    );
    println!(
        "  Subtitle languages: [{}]",
        compiled.config.subtitle_languages.join(", ")
    );
    println!("  On error: {:?}", compiled.config.on_error);
    println!();

    // Phases
    println!(
        "{} (order: [{}])",
        "Phases:".bold(),
        compiled.phase_order.join(" → ")
    );
    for phase in &compiled.phases {
        println!("  {} {}", "▸".cyan(), phase.name.bold());
        if !phase.depends_on.is_empty() {
            println!("    depends_on: [{}]", phase.depends_on.join(", "));
        }
        if phase.skip_when.is_some() {
            println!("    skip_when: (condition)");
        }
        if phase.run_if.is_some() {
            println!("    run_if: (condition)");
        }
        println!("    on_error: {:?}", phase.on_error);
        println!("    operations: {}", phase.operations.len());
    }

    // JSON output for full details
    println!();
    println!(
        "{}",
        serde_json::to_string_pretty(&compiled).unwrap_or_else(|_| "Failed to serialize".into())
    );

    Ok(())
}

async fn format(file: std::path::PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    let ast = voom_dsl::parse_policy(&source).map_err(|e| anyhow::anyhow!("{e}"))?;
    let formatted = voom_dsl::format_policy(&ast);

    std::fs::write(&file, &formatted)
        .with_context(|| format!("Failed to write: {}", file.display()))?;

    println!(
        "{} Formatted {}",
        "OK".bold().green(),
        file.display().to_string().cyan()
    );

    Ok(())
}
