use anyhow::Result;
use console::style;
use voom_ffmpeg_executor::hwaccel::{resolve_hw_config, HwAccelBackend};
use voom_ffmpeg_executor::probe::{
    enumerate_gpus, parse_hw_implementations, parse_hwaccels, validate_hw_encoder,
    validate_hw_encoder_on_device, GpuDevice,
};

use crate::app;
use crate::config;
use crate::output::sanitize_for_display;
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

fn gpu_section_header(backend: HwAccelBackend) -> &'static str {
    match backend {
        HwAccelBackend::Nvenc => "GPUs",
        HwAccelBackend::Vaapi | HwAccelBackend::Qsv => "Render devices",
        HwAccelBackend::Videotoolbox => "Devices",
    }
}

fn gpu_display_label(device: &GpuDevice, backend: HwAccelBackend) -> String {
    match backend {
        HwAccelBackend::Nvenc => {
            let vram = device
                .vram_mib
                .map(|m| format!(" ({m} MiB)"))
                .unwrap_or_default();
            format!("GPU {}: {}{}", device.id, device.name, vram)
        }
        _ => {
            if device.name == device.id {
                device.id.clone()
            } else {
                format!("{} ({})", device.id, device.name)
            }
        }
    }
}

fn encoder_block_label(device: &GpuDevice, backend: HwAccelBackend) -> String {
    match backend {
        HwAccelBackend::Nvenc => {
            format!("GPU {} — {}", device.id, device.name)
        }
        _ => device.id.clone(),
    }
}

fn print_encoder_block(hw_encoders: &[String], backend: HwAccelBackend, device: &GpuDevice) {
    let label = encoder_block_label(device, backend);
    println!();
    println!("  HW Encoders ({label}):");
    for enc in hw_encoders {
        if validate_hw_encoder_on_device(enc, backend, device) {
            println!("    {:<20}{}", enc, style("OK (device validated)").green());
        } else {
            println!("    {:<20}{}", enc, style("UNSUPPORTED").yellow());
        }
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

    if !hw_accels.is_empty() {
        println!("  Available ... {}", hw_accels.join(", "));
    }

    let app_config = config::load_config().unwrap_or_default();
    let hw_accel_override = app_config
        .plugin
        .get("ffmpeg-executor")
        .and_then(|t| t.get("hw_accel"))
        .and_then(|v| v.as_str());

    let (config, source) = resolve_hw_config(hw_accel_override, &hw_accels);

    let backend = match config.backend {
        Some(backend) => {
            println!(
                "  Backend ... {} {}",
                style(backend_label(backend)).green(),
                style(format!("({source})")).dim()
            );
            backend
        }
        None => {
            if source == "disabled" {
                println!(
                    "  Backend ... {} {}",
                    style("disabled").yellow(),
                    style("(config override)").dim()
                );
            } else {
                println!("  Backend ... {}", style("none detected").yellow());
            }
            return;
        }
    };

    let devices = enumerate_gpus(backend);
    if !devices.is_empty() {
        println!();
        println!("  {}:", gpu_section_header(backend));
        for device in &devices {
            println!("    {}", gpu_display_label(device, backend));
        }
    }

    // Show configured GPU device from config
    if let Some(device_id) = app_config
        .plugin
        .get("ffmpeg-executor")
        .and_then(|t| t.get("gpu_device"))
        .and_then(|v| v.as_str())
    {
        let found = devices.iter().any(|d| d.id == device_id);
        let safe_id = sanitize_for_display(device_id);
        if found {
            println!(
                "  Configured GPU ... {} {}",
                style(&safe_id).cyan(),
                style("(found)").green()
            );
        } else {
            println!(
                "  Configured GPU ... {} {}",
                style(&safe_id).cyan(),
                style("(NOT FOUND)").red()
            );
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
            if devices.is_empty() {
                // Fallback: no GPU enumeration, single block
                println!();
                println!("  HW Encoders:");
                for enc in &hw_encoders {
                    if validate_hw_encoder(enc) {
                        println!("    {:<20}{}", enc, style("OK (device validated)").green());
                    } else {
                        println!("    {:<20}{}", enc, style("UNSUPPORTED").yellow());
                    }
                }
            } else {
                for device in &devices {
                    print_encoder_block(&hw_encoders, backend, device);
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
