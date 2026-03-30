use anyhow::Result;
use console::style;
use voom_domain::utils::format;

use crate::app;
use crate::cli::{HistoryArgs, OutputFormat};
use crate::output;

pub fn run(args: HistoryArgs) -> Result<()> {
    let config = crate::config::load_config()?;
    let store = app::open_store(&config)?;

    let path = args.file.canonicalize().unwrap_or(args.file.clone());
    let entries = store.file_history(&path)?;

    if entries.is_empty() {
        println!(
            "{}",
            style(format!("No history found for {}", path.display())).dim()
        );
        return Ok(());
    }

    match args.format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id.to_string(),
                        "file_id": e.file_id.to_string(),
                        "path": e.path.display().to_string(),
                        "container": e.container.as_str(),
                        "track_count": e.track_count,
                        "content_hash": e.content_hash,
                        "introspected_at": e.introspected_at.to_rfc3339(),
                        "archived_at": e.archived_at.to_rfc3339(),
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
            println!(
                "{} for {}:\n",
                style(format!("{} history entries", entries.len())).bold(),
                style(path.display()).cyan()
            );

            let mut table = output::new_table();
            table.set_header(vec!["#", "Date", "Container", "Tracks", "Hash"]);

            for (i, entry) in entries.iter().enumerate() {
                let date = format::format_display(&entry.archived_at);
                let hash = entry
                    .content_hash
                    .as_deref()
                    .map(output::hash_preview)
                    .unwrap_or("—");

                table.add_row(vec![
                    comfy_table::Cell::new(i + 1),
                    comfy_table::Cell::new(date),
                    comfy_table::Cell::new(entry.container.as_str()),
                    comfy_table::Cell::new(entry.track_count),
                    comfy_table::Cell::new(hash),
                ]);
            }

            println!("{table}");
        }
    }

    Ok(())
}
