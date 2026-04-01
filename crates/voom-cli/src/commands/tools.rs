use anyhow::Result;
use console::style;

use crate::cli::{OutputFormat, ToolsCommands};
use crate::output;
use crate::output::sanitize_for_display;

pub fn run(cmd: ToolsCommands) -> Result<()> {
    match cmd {
        ToolsCommands::List { format } => list(format),
        ToolsCommands::Info { name, format } => info(name, format),
    }
}

fn list(format: OutputFormat) -> Result<()> {
    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let mut tools: Vec<_> = detector.detected_tools().values().cloned().collect();
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    if tools.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!("{}", style("No external tools detected.").dim());
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "version": t.version,
                        "path": t.path.display().to_string(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .expect("serde_json::Value serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            let mut table = output::new_table();
            table.set_header(vec!["Tool", "Version", "Path"]);
            for t in &tools {
                table.add_row(vec![
                    comfy_table::Cell::new(&t.name),
                    comfy_table::Cell::new(sanitize_for_display(&t.version)),
                    comfy_table::Cell::new(t.path.display()),
                ]);
            }
            println!("{} tool(s) detected:\n", style(tools.len()).bold());
            println!("{table}");
        }
        OutputFormat::Plain => {
            for t in &tools {
                println!("{}", t.name);
            }
        }
    }

    Ok(())
}

fn info(name: String, format: OutputFormat) -> Result<()> {
    let mut detector = voom_tool_detector::ToolDetectorPlugin::new();
    detector.detect_all();

    let tool = match detector.tool(&name) {
        Some(t) => t.clone(),
        None => {
            let available: Vec<_> = detector
                .detected_tools()
                .values()
                .map(|t| t.name.as_str())
                .collect();
            if available.is_empty() {
                anyhow::bail!("Tool \"{name}\" not found. No tools detected.");
            }
            anyhow::bail!(
                "Tool \"{name}\" not found. Available: {}",
                available.join(", ")
            );
        }
    };

    // Try to load executor capabilities for executor tools
    let executor_name = match name.as_str() {
        "ffmpeg" | "ffprobe" => Some("ffmpeg-executor"),
        "mkvmerge" | "mkvpropedit" | "mkvextract" => Some("mkvtoolnix-executor"),
        _ => None,
    };

    let capabilities = executor_name.and_then(|exec_name| {
        let snapshot = collect_executor_capabilities(exec_name)?;
        if snapshot.executor_capabilities(exec_name).is_some() {
            Some((exec_name.to_string(), snapshot))
        } else {
            None
        }
    });

    match format {
        OutputFormat::Json => {
            let mut json = serde_json::json!({
                "name": tool.name,
                "version": tool.version,
                "path": tool.path.display().to_string(),
            });
            if let Some((ref exec_name, ref caps)) = capabilities {
                if let Some(exec_caps) = caps.executor_capabilities(exec_name) {
                    json["executor"] = serde_json::json!({
                        "plugin": exec_name,
                        "hw_accels": exec_caps.hw_accels,
                        "codecs": {
                            "decoders": exec_caps.codecs.decoders,
                            "encoders": exec_caps.codecs.encoders,
                            "hw_decoders": exec_caps.codecs.hw_decoders,
                            "hw_encoders": exec_caps.codecs.hw_encoders,
                        },
                        "formats": exec_caps.formats,
                    });
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .expect("serde_json::Value serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            println!("{} {}", style("Tool:").bold(), style(&tool.name).cyan());
            println!(
                "{} {}",
                style("Version:").bold(),
                sanitize_for_display(&tool.version)
            );
            println!("{} {}", style("Path:").bold(), tool.path.display());

            if let Some((ref exec_name, ref caps)) = capabilities {
                println!();
                println!(
                    "{} {}",
                    style("Executor plugin:").bold(),
                    style(exec_name).cyan()
                );
                output::format_executor_capabilities(exec_name, caps);
            }
        }
        OutputFormat::Plain => {
            println!("name\t{}", tool.name);
            println!("version\t{}", sanitize_for_display(&tool.version));
            println!("path\t{}", tool.path.display());
        }
    }

    Ok(())
}

/// Bootstrap a minimal kernel with just the named executor plugin and a
/// capability collector, avoiding the overhead of a full kernel + SQLite.
fn collect_executor_capabilities(
    exec_name: &str,
) -> Option<voom_domain::capability_map::CapabilityMap> {
    use std::sync::Arc;
    use voom_kernel::{Kernel, PluginContext};

    let config = crate::config::load_config().ok()?;
    let plugin_json = config
        .plugin
        .get(exec_name)
        .and_then(|t| serde_json::to_value(t).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let ctx = PluginContext::new(plugin_json, config.data_dir.clone());
    let collector = Arc::new(crate::capability_collector::CapabilityCollectorPlugin::new());

    let mut kernel = Kernel::new();
    kernel.register_plugin(collector.clone(), 1).ok()?;

    let executor: Arc<dyn voom_kernel::Plugin> = match exec_name {
        "ffmpeg-executor" => Arc::new(voom_ffmpeg_executor::FfmpegExecutorPlugin::new()),
        "mkvtoolnix-executor" => {
            Arc::new(voom_mkvtoolnix_executor::MkvtoolnixExecutorPlugin::new())
        }
        _ => return None,
    };

    kernel.init_and_register(executor, 10, &ctx).ok()?;
    Some(collector.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_does_not_panic() {
        let result = list(OutputFormat::Table);
        assert!(result.is_ok());
    }

    #[test]
    fn test_info_nonexistent_tool_fails() {
        let result = info("nonexistent-tool-xyz".to_string(), OutputFormat::Table);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
