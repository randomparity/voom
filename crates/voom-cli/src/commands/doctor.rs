use anyhow::Result;
use console::style;
use voom_ffmpeg_executor::hwaccel::{HwAccelBackend, HwAccelConfig};
use voom_ffmpeg_executor::probe::{parse_hw_implementations, parse_hwaccels, validate_hw_encoder};

use crate::app;
use crate::config;
use crate::tools::print_tool_status;

/// Run the doctor command.
///
/// Tool detection creates a standalone `ToolDetectorPlugin` instance rather
/// than retrieving the kernel-registered one. This is intentional: doctor
/// must be able to diagnose tool availability even when the kernel fails to
/// bootstrap (e.g. missing database directory). The standalone instance does
/// not receive per-plugin configuration from config.toml, but tool-detector
/// currently has no configurable settings.
pub fn run() -> Result<()> {
    println!("{}", style("VOOM System Health Check").bold().underlined());
    println!();

    let mut issues = 0u32;

    // 1. Config
    print!("  Config file ... ");
    let config_path = config::config_path();
    if config_path.exists() {
        match config::load_config() {
            Ok(_) => println!("{}", style("OK").green()),
            Err(e) => {
                println!("{} {e}", style("ERROR").red());
                issues += 1;
            }
        }
    } else {
        println!("{} (using defaults)", style("not found").yellow());
    }

    // 2. Database
    print!("  Database ... ");
    let config = config::load_config().unwrap_or_default();
    let kernel_result = app::bootstrap_kernel_with_store(&config);
    match &kernel_result {
        Ok(app::BootstrapResult { store, .. }) => {
            let mut doctor_filters = voom_domain::FileFilters::default();
            doctor_filters.limit = Some(1);
            match store.list_files(&doctor_filters) {
                Ok(_) => println!("{}", style("OK").green()),
                Err(e) => {
                    println!("{} {e}", style("ERROR").red());
                    issues += 1;
                }
            }
        }
        Err(e) => {
            println!("{} {e}", style("ERROR").red());
            issues += 1;
        }
    }

    // 3. External tools
    println!();
    println!("{}", style("External tools:").bold());

    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let tool_result = print_tool_status(&detector);
    issues += tool_result.missing_required;

    // 4. Hardware acceleration (only if ffmpeg was found)
    if detector.tool("ffmpeg").is_some() {
        print_hw_accel_status();
    }

    // 5. Plugins
    println!();
    println!("{}", style("Plugins:").bold());
    if let Ok(app::BootstrapResult { kernel, .. }) = &kernel_result {
        let names = kernel.registry.plugin_names();
        println!("  {} plugins registered", style(names.len()).green());
        for name in &names {
            println!("    - {name}");
        }
    }

    // Summary
    println!();
    if issues == 0 {
        println!("{}", style("All checks passed.").bold().green());
    } else {
        println!(
            "{} {} issue(s) found.",
            style("WARNING").bold().yellow(),
            issues
        );
    }

    Ok(())
}

fn backend_label(backend: HwAccelBackend) -> &'static str {
    match backend {
        HwAccelBackend::Nvenc => "NVENC (cuda)",
        HwAccelBackend::Qsv => "QuickSync (qsv)",
        HwAccelBackend::Vaapi => "VA-API (vaapi)",
        HwAccelBackend::Videotoolbox => "VideoToolbox",
    }
}

fn print_hw_accel_status() {
    println!();
    println!("{}", style("Hardware acceleration:").bold());

    let hwaccels_output = std::process::Command::new("ffmpeg")
        .args(["-hwaccels", "-hide_banner"])
        .output();

    let hw_accels = match hwaccels_output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_hwaccels(&stdout)
        }
        Err(_) => return,
    };

    let config = HwAccelConfig::from_probed(&hw_accels);
    match config.backend {
        Some(backend) => {
            println!("  Backend ... {}", style(backend_label(backend)).green());
        }
        None => {
            println!("  Backend ... {}", style("none detected").yellow());
            return;
        }
    }

    let encoders_output = std::process::Command::new("ffmpeg")
        .args(["-encoders", "-hide_banner"])
        .output();
    let decoders_output = std::process::Command::new("ffmpeg")
        .args(["-decoders", "-hide_banner"])
        .output();

    if let Ok(output) = encoders_output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let hw_encoders = parse_hw_implementations(&stdout);
        if !hw_encoders.is_empty() {
            println!();
            println!("  HW Encoders:");
            for enc in &hw_encoders {
                if validate_hw_encoder(enc) {
                    println!("    {:<20}{}", enc, style("OK (device validated)").green());
                } else {
                    println!(
                        "    {:<20}{}",
                        enc,
                        style("UNSUPPORTED (not available on this device)").yellow()
                    );
                }
            }
        }
    }

    if let Ok(output) = decoders_output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let hw_decoders = parse_hw_implementations(&stdout);
        if !hw_decoders.is_empty() {
            println!();
            println!("  HW Decoders:");
            for dec in &hw_decoders {
                println!("    {:<20}{}", dec, style("available").green());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_tool_detector_creation() {
        let detector = voom_tool_detector::ToolDetectorPlugin::new();
        // Should be able to create without panic
        assert!(detector.tool("nonexistent-tool").is_none());
    }
}
