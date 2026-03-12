use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;

pub async fn run() -> Result<()> {
    println!("{}", "VOOM First-Time Setup".bold().underline());
    println!();

    let config = app::AppConfig::default();
    let config_path = app::config_path();

    // 1. Create config directory
    let config_dir = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent directory"))?;
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
        println!(
            "  {} Created {}",
            "OK".green(),
            config_dir.display().to_string().cyan()
        );
    } else {
        println!(
            "  {} {} already exists",
            "OK".green(),
            config_dir.display().to_string().dimmed()
        );
    }

    // 2. Create data directory
    if !config.data_dir.exists() {
        std::fs::create_dir_all(&config.data_dir)?;
        println!(
            "  {} Created {}",
            "OK".green(),
            config.data_dir.display().to_string().cyan()
        );
    } else {
        println!(
            "  {} {} already exists",
            "OK".green(),
            config.data_dir.display().to_string().dimmed()
        );
    }

    // 3. Create policies directory
    let policies_dir = config_dir.join("policies");
    if !policies_dir.exists() {
        std::fs::create_dir_all(&policies_dir)?;
        println!(
            "  {} Created {}",
            "OK".green(),
            policies_dir.display().to_string().cyan()
        );
    }

    // 4. Create default config if missing
    if !config_path.exists() {
        let contents = toml::to_string_pretty(&config)?;
        std::fs::write(&config_path, contents)?;
        println!(
            "  {} Created {}",
            "OK".green(),
            config_path.display().to_string().cyan()
        );
    } else {
        println!(
            "  {} {} already exists",
            "OK".green(),
            config_path.display().to_string().dimmed()
        );
    }

    // 5. Initialize database
    print!("  Database ... ");
    match app::bootstrap_kernel(&config) {
        Ok(_) => println!("{}", "OK".green()),
        Err(e) => println!("{} {e}", "ERROR".red()),
    }

    // 6. Check tools
    println!();
    println!("{}", "Checking external tools:".bold());
    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    for tool in ["ffprobe", "ffmpeg", "mkvmerge", "mkvpropedit"] {
        print!("  {tool} ... ");
        if let Some(t) = detector.get_tool(tool) {
            println!("{} ({})", "found".green(), t.version.dimmed());
        } else {
            println!("{}", "not found".red());
        }
    }

    println!();
    println!("{}", "Setup complete! You can now:".bold().green());
    println!("  voom scan <path>              Scan a media directory");
    println!("  voom inspect <file>           Inspect a media file");
    println!("  voom policy validate <file>   Validate a policy");
    println!("  voom doctor                   Run health checks");

    Ok(())
}
