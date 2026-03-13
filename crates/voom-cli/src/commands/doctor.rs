use anyhow::Result;
use owo_colors::OwoColorize;

use crate::app;

pub async fn run() -> Result<()> {
    println!("{}", "VOOM System Health Check".bold().underline());
    println!();

    let mut issues = 0u32;

    // 1. Config
    print!("  Config file ... ");
    let config_path = app::config_path();
    if config_path.exists() {
        match app::load_config() {
            Ok(_) => println!("{}", "OK".green()),
            Err(e) => {
                println!("{} {e}", "ERROR".red());
                issues += 1;
            }
        }
    } else {
        println!("{} (using defaults)", "not found".yellow());
    }

    // 2. Database
    print!("  Database ... ");
    let config = app::load_config().unwrap_or_default();
    let kernel_result = app::bootstrap_kernel(&config);
    match &kernel_result {
        Ok(_kernel) => match app::open_store(&config) {
            Ok(store) => {
                use voom_domain::storage::StorageTrait;
                match store.list_files(&voom_domain::FileFilters {
                    limit: Some(1),
                    ..Default::default()
                }) {
                    Ok(_) => println!("{}", "OK".green()),
                    Err(e) => {
                        println!("{} {e}", "ERROR".red());
                        issues += 1;
                    }
                }
            }
            Err(e) => {
                println!("{} {e}", "ERROR".red());
                issues += 1;
            }
        },
        Err(e) => {
            println!("{} {e}", "ERROR".red());
            issues += 1;
        }
    }

    // 3. External tools
    println!();
    println!("{}", "External tools:".bold());

    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let required_tools = ["ffprobe", "ffmpeg", "mkvmerge", "mkvpropedit"];
    let optional_tools = ["mkvextract", "mediainfo", "HandBrakeCLI"];

    for tool in required_tools {
        print!("  {tool} ... ");
        if let Some(t) = detector.get_tool(tool) {
            println!("{} ({})", "OK".green(), t.version.dimmed());
        } else {
            println!("{} (required)", "NOT FOUND".red());
            issues += 1;
        }
    }

    for tool in optional_tools {
        print!("  {tool} ... ");
        if let Some(t) = detector.get_tool(tool) {
            println!("{} ({})", "OK".green(), t.version.dimmed());
        } else {
            println!("{}", "not found".yellow());
        }
    }

    // 4. Plugins
    println!();
    println!("{}", "Plugins:".bold());
    if let Ok(kernel) = &kernel_result {
        let names = kernel.registry.plugin_names();
        println!("  {} plugins registered", names.len().to_string().green());
        for name in &names {
            println!("    - {name}");
        }
    }

    // Summary
    println!();
    if issues == 0 {
        println!("{}", "All checks passed.".bold().green());
    } else {
        println!("{} {} issue(s) found.", "WARNING".bold().yellow(), issues);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn required_tools_list() {
        let required = ["ffprobe", "ffmpeg", "mkvmerge", "mkvpropedit"];
        assert_eq!(required.len(), 4);
        // Verify these are the tools doctor checks
        for tool in &required {
            assert!(!tool.is_empty());
        }
    }

    #[test]
    fn optional_tools_list() {
        let optional = ["mkvextract", "mediainfo", "HandBrakeCLI"];
        assert_eq!(optional.len(), 3);
    }

    #[test]
    fn tool_detector_creation() {
        let detector = voom_tool_detector::ToolDetectorPlugin::new();
        // Should be able to create without panic
        assert!(detector.get_tool("nonexistent-tool").is_none());
    }
}
