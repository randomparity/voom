use anyhow::{Context, Result};
use console::style;

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
    let config_dir = crate::config::voom_config_dir().join("policies");

    if !config_dir.exists() {
        println!("{}", style("No policies directory found.").dim());
        println!("Create policies in: {}", style(config_dir.display()).cyan());
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(&config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "voom") {
            let name = path
                .file_stem()
                .expect("file has .voom extension so stem exists")
                .to_string_lossy();
            match voom_dsl::compile(&std::fs::read_to_string(&path)?) {
                Ok(policy) => {
                    println!(
                        "  {} {} ({} phases)",
                        style("OK").green(),
                        style(&policy.name).bold(),
                        policy.phases.len()
                    );
                }
                Err(e) => {
                    println!("  {} {} — {e}", style("ERR").red(), style(&name).bold());
                }
            }
            found = true;
        }
    }

    if !found {
        println!("{}", style("No .voom policy files found.").dim());
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
                style("OK").bold().green(),
                policy.name,
                policy.phases.len(),
                policy.phase_order.len(),
                policy.phase_order.join(", ")
            );
        }
        Err(e) => {
            anyhow::bail!("Policy validation failed: {e}");
        }
    }

    Ok(())
}

async fn show(file: std::path::PathBuf) -> Result<()> {
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    let compiled = voom_dsl::compile(&source)
        .map_err(|e| anyhow::anyhow!("policy compilation failed: {e}"))?;

    println!(
        "{} \"{}\"",
        style("Policy").bold(),
        style(&compiled.name).cyan()
    );
    println!();

    // Config
    println!("{}", style("Config:").bold());
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
        style("Phases:").bold(),
        compiled.phase_order.join(" → ")
    );
    for phase in &compiled.phases {
        println!("  {} {}", style("▸").cyan(), style(&phase.name).bold());
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

    let ast =
        voom_dsl::parse_policy(&source).map_err(|e| anyhow::anyhow!("policy parse failed: {e}"))?;
    let formatted = voom_dsl::format_policy(&ast);

    std::fs::write(&file, &formatted)
        .with_context(|| format!("Failed to write: {}", file.display()))?;

    println!(
        "{} Formatted {}",
        style("OK").bold().green(),
        style(file.display()).cyan()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_POLICY: &str = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
}
"#;

    #[tokio::test]
    async fn validate_valid_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        // validate reads the file and calls voom_dsl::compile
        let result = validate(file).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn validate_invalid_policy_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.voom");
        std::fs::write(&file, "not a valid policy at all").unwrap();

        // validate() returns Err for invalid policies, so we test indirectly via compile
        let source = std::fs::read_to_string(&file).unwrap();
        assert!(voom_dsl::compile(&source).is_err());
    }

    #[tokio::test]
    async fn validate_nonexistent_file_returns_error() {
        let result = validate(std::path::PathBuf::from("/nonexistent/test.voom")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn format_valid_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = format(file.clone()).await;
        assert!(result.is_ok());

        // Verify the file was rewritten
        let formatted = std::fs::read_to_string(&file).unwrap();
        assert!(formatted.contains("policy"));
        assert!(formatted.contains("test-policy"));
    }

    #[tokio::test]
    async fn format_nonexistent_file_returns_error() {
        let result = format(std::path::PathBuf::from("/nonexistent/test.voom")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn show_valid_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = show(file).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn show_nonexistent_file_returns_error() {
        let result = show(std::path::PathBuf::from("/nonexistent/test.voom")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn show_invalid_policy_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.voom");
        std::fs::write(&file, "garbage content here").unwrap();

        let result = show(file).await;
        assert!(result.is_err());
    }
}
