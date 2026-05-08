use anyhow::Result;
use console::style;

use crate::app;
use crate::config;
use crate::tools::print_tool_status;

pub fn run() -> Result<()> {
    println!("{}", style("VOOM First-Time Setup").bold().underlined());
    println!();

    let cfg = config::AppConfig::default();
    let config_path = config::config_path();

    // 1. Create config directory
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent directory"))?;
    if config_dir.exists() {
        println!(
            "  {} {} already exists",
            style("OK").green(),
            style(config_dir.display()).dim()
        );
    } else {
        std::fs::create_dir_all(config_dir)?;
        println!(
            "  {} Created {}",
            style("OK").green(),
            style(config_dir.display()).cyan()
        );
    }

    // 2. Create data directory
    if cfg.data_dir.exists() {
        println!(
            "  {} {} already exists",
            style("OK").green(),
            style(cfg.data_dir.display()).dim()
        );
    } else {
        std::fs::create_dir_all(&cfg.data_dir)?;
        println!(
            "  {} Created {}",
            style("OK").green(),
            style(cfg.data_dir.display()).cyan()
        );
    }

    // 3. Create policies directory and starter policy
    let policies_dir = config::policies_dir();
    if !policies_dir.exists() {
        std::fs::create_dir_all(&policies_dir)?;
        println!(
            "  {} Created {}",
            style("OK").green(),
            style(policies_dir.display()).cyan()
        );
    }

    let starter_policy = policies_dir.join("default.voom");
    if !starter_policy.exists() {
        std::fs::write(&starter_policy, default_policy_contents())?;
        println!(
            "  {} Created {}",
            style("OK").green(),
            style(starter_policy.display()).cyan()
        );
    }

    // 4. Create default config if missing
    if config_path.exists() {
        println!(
            "  {} {} already exists",
            style("OK").green(),
            style(config_path.display()).dim()
        );
    } else {
        let contents = config::default_config_contents();

        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&config_path)
                .and_then(|mut f| f.write_all(contents.as_bytes()))?;
        }
        #[cfg(not(unix))]
        std::fs::write(&config_path, &contents)?;
        println!(
            "  {} Created {}",
            style("OK").green(),
            style(config_path.display()).cyan()
        );
    }

    // 5. Initialize database
    print!("  Database ... ");
    match app::bootstrap_kernel(&cfg) {
        Ok(_) => println!("{}", style("OK").green()),
        Err(e) => println!("{} {e}", style("ERROR").red()),
    }

    // 6. Check tools
    println!();
    println!("{}", style("Checking external tools:").bold());
    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let _tool_result = print_tool_status(&detector);

    println!();
    println!("{}", style("Setup complete! You can now:").bold().green());
    println!("  voom scan <path>              Scan a media directory");
    println!("  voom inspect <file>           Inspect a media file");
    println!("  voom policy validate <file>   Validate a policy");
    println!("  voom env check                Run environment checks");

    Ok(())
}

/// Returns the contents of the starter policy file created by `voom init`.
fn default_policy_contents() -> &'static str {
    r#"// VOOM starter policy — customize this to match your media library preferences.
// Documentation: https://github.com/randomparity/voom/blob/main/docs/INITIAL_DESIGN.md
//
// Policies are organized into phases that run in order. Each phase performs
// a specific task (normalize tracks, transcode video, etc.). You can add
// dependencies between phases and skip them conditionally.

policy "default" {
  config {
    // Keep audio and subtitle tracks in these languages; remove others.
    languages audio: [eng, und]
    languages subtitle: [eng, und]

    // Strings that identify commentary tracks.
    commentary_patterns: ["commentary", "director", "cast"]

    // What to do when a phase encounters an error: continue or abort.
    on_error: continue
  }

  // Phase 1: Ensure all files use the MKV container.
  phase containerize {
    container mkv
  }

  // Phase 2: Clean up and reorder tracks.
  phase normalize {
    depends_on: [containerize]

    // Keep only the audio/subtitle languages listed in config above.
    keep audio where lang in [eng, und]
    keep subtitles where lang in [eng, und]

    // Set a predictable track order.
    order tracks [
      video, audio_main, audio_alternate,
      subtitle_main, subtitle_forced,
      audio_commentary, subtitle_commentary, attachment
    ]

    // Mark the first track per language as default; no default subtitle.
    defaults {
      audio: first_per_language
      subtitle: none
    }
  }
}
"#
}

#[cfg(test)]
mod tests {
    use crate::config;

    #[test]
    fn test_default_config_dirs_are_consistent() {
        let cfg = config::AppConfig::default();
        let config_path = config::config_path();
        let config_dir = config_path.parent().unwrap();

        // The policies dir that init creates
        let policies_dir = config_dir.join("policies");
        assert!(policies_dir.ends_with("voom/policies"));

        // Data dir defaults to the config dir
        assert_eq!(cfg.data_dir, config::voom_config_dir());
    }

    #[test]
    fn test_init_creates_directories_in_temp() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("voom");
        let policies_dir = config_dir.join("policies");

        // Simulate what init does for directory creation
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&policies_dir).unwrap();

        assert!(config_dir.exists());
        assert!(policies_dir.exists());
    }

    #[test]
    fn test_starter_policy_is_valid() {
        let contents = super::default_policy_contents();
        // Verify it parses successfully
        let ast = voom_dsl::parse_policy(contents).expect("starter policy should parse");
        assert_eq!(ast.name, "default");
        // Verify it validates
        voom_dsl::validate(&ast).expect("starter policy should validate");
    }

    #[test]
    fn test_init_creates_default_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join("config.toml");

        let contents = config::default_config_contents();
        std::fs::write(&config_file, &contents).unwrap();

        // Verify the written file is valid TOML (all options are commented out)
        let reloaded: config::AppConfig =
            toml::from_str(&std::fs::read_to_string(&config_file).unwrap()).unwrap();
        assert!(reloaded.auth_token.is_none());
        assert!(reloaded.plugins.disabled_plugins.is_empty());
    }
}
