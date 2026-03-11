//! Output formatting utilities for the CLI.

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;
use voom_domain::media::{MediaFile, Track};
use voom_domain::plan::Plan;
use voom_domain::utils::datetime;

use crate::cli::OutputFormat;

/// Format a list of discovered files as a table.
pub fn format_scan_results(files: &[(std::path::PathBuf, u64, String)], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = files
                .iter()
                .map(|(path, size, hash)| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "size": size,
                        "hash": hash,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        OutputFormat::Table => {
            let mut table = new_table();
            table.set_header(vec!["Path", "Size", "Hash"]);
            for (path, size, hash) in files {
                table.add_row(vec![
                    Cell::new(path.display()),
                    Cell::new(datetime::format_size(*size)),
                    Cell::new(&hash[..12]),
                ]);
            }
            println!("{table}");
        }
    }
}

/// Format a media file's metadata as a table.
pub fn format_file_info(file: &MediaFile, tracks_only: bool) {
    if !tracks_only {
        println!("{}", "File Information".bold());
        let mut table = new_table();
        table.set_header(vec!["Property", "Value"]);
        table.add_row(vec!["Path", &file.path.display().to_string()]);
        table.add_row(vec!["Container", file.container.as_str()]);
        table.add_row(vec!["Size", &datetime::format_size(file.size)]);
        table.add_row(vec!["Duration", &datetime::format_duration(file.duration)]);
        if let Some(br) = file.bitrate {
            table.add_row(vec!["Bitrate", &format!("{} kbps", br / 1000)]);
        }
        table.add_row(vec!["Hash", &file.content_hash]);
        table.add_row(vec!["ID", &file.id.to_string()]);
        println!("{table}");
        println!();
    }

    println!("{}", "Tracks".bold());
    format_tracks(&file.tracks);
}

/// Format a media file as JSON.
pub fn format_file_json(file: &MediaFile) {
    println!("{}", serde_json::to_string_pretty(file).unwrap());
}

/// Format tracks as a table.
pub fn format_tracks(tracks: &[Track]) {
    let mut table = new_table();
    table.set_header(vec![
        "#", "Type", "Codec", "Language", "Title", "Default", "Forced", "Details",
    ]);

    for track in tracks {
        let details = track_details(track);
        table.add_row(vec![
            Cell::new(track.index),
            Cell::new(track.track_type.as_str()),
            Cell::new(&track.codec),
            Cell::new(&track.language),
            Cell::new(&track.title),
            Cell::new(if track.is_default { "yes" } else { "" }),
            Cell::new(if track.is_forced { "yes" } else { "" }),
            Cell::new(details),
        ]);
    }

    println!("{table}");
}

/// Build a details string for a track (resolution, channels, etc.).
fn track_details(track: &Track) -> String {
    let mut parts = Vec::new();

    if let (Some(w), Some(h)) = (track.width, track.height) {
        parts.push(format!("{w}x{h}"));
    }
    if let Some(fps) = track.frame_rate {
        parts.push(format!("{fps:.2}fps"));
    }
    if track.is_hdr {
        if let Some(ref fmt) = track.hdr_format {
            parts.push(fmt.clone());
        } else {
            parts.push("HDR".into());
        }
    }
    if let Some(ch) = track.channels {
        parts.push(format!("{ch}ch"));
    }
    if let Some(ref layout) = track.channel_layout {
        parts.push(layout.clone());
    }
    if let Some(sr) = track.sample_rate {
        parts.push(format!("{sr}Hz"));
    }

    parts.join(", ")
}

/// Format plan output for dry-run display.
pub fn format_plans(plans: &[Plan]) {
    for plan in plans {
        println!("\n{} {}", "Phase:".bold(), plan.phase_name.bold().cyan());

        if let Some(ref reason) = plan.skip_reason {
            println!("  {} {reason}", "SKIPPED:".yellow());
            continue;
        }

        if plan.actions.is_empty() {
            println!("  {}", "No actions needed.".dimmed());
        } else {
            for (i, action) in plan.actions.iter().enumerate() {
                println!(
                    "  {}. {} {}",
                    (i + 1).to_string().bold(),
                    action.operation.as_str().green(),
                    action.description
                );
            }
        }

        for warning in &plan.warnings {
            println!("  {} {warning}", "WARNING:".yellow().bold());
        }
    }
}

/// Format a list of plugins as a table.
pub fn format_plugin_list(plugins: &[(String, String, Vec<String>)]) {
    let mut table = new_table();
    table.set_header(vec!["Plugin", "Version", "Capabilities"]);
    for (name, version, caps) in plugins {
        table.add_row(vec![
            Cell::new(name),
            Cell::new(version),
            Cell::new(caps.join(", ")),
        ]);
    }
    println!("{table}");
}

/// Create a table with the standard VOOM style.
fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}
