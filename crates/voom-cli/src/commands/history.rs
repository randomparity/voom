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

    // Look up by file identity first to capture lineage across renames.
    // Fall back to path-based lookup for files not in the database
    // (e.g., deleted files where only transition records remain).
    let transitions = match store
        .file_by_path(&path)
        .map_err(|e| anyhow::anyhow!("failed to look up file: {e}"))?
    {
        Some(file) => store
            .transitions_for_file(&file.id)
            .map_err(|e| anyhow::anyhow!("failed to retrieve transitions: {e}"))?,
        None => store
            .transitions_for_path(&path)
            .map_err(|e| anyhow::anyhow!("failed to retrieve transitions: {e}"))?,
    };

    if transitions.is_empty() {
        if args.format.is_machine() {
            if matches!(args.format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style(format!("No history found for {}", path.display())).dim()
        );
        return Ok(());
    }

    match args.format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = transitions
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "id": t.id.to_string(),
                        "file_id": t.file_id.to_string(),
                        "path": t.path.display().to_string(),
                        "from_hash": t.from_hash,
                        "to_hash": t.to_hash,
                        "from_size": t.from_size,
                        "to_size": t.to_size,
                        "source": t.source.as_str(),
                        "source_detail": t.source_detail,
                        "plan_id": t.plan_id.map(|id| id.to_string()),
                        "created_at": t.created_at.to_rfc3339(),
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
                style(format!("{} transition entries", transitions.len())).bold(),
                style(path.display()).cyan()
            );

            let mut table = output::new_table();
            table.set_header(vec!["#", "Date", "Source", "From Hash", "To Hash"]);

            for (i, t) in transitions.iter().enumerate() {
                let date = format::format_display(&t.created_at);
                let from = t
                    .from_hash
                    .as_deref()
                    .map(output::hash_preview)
                    .unwrap_or("—");
                let to = output::hash_preview(&t.to_hash);

                table.add_row(vec![
                    comfy_table::Cell::new(i + 1),
                    comfy_table::Cell::new(date),
                    comfy_table::Cell::new(t.source.as_str()),
                    comfy_table::Cell::new(from),
                    comfy_table::Cell::new(to),
                ]);
            }

            println!("{table}");
        }
        OutputFormat::Plain => {
            for t in &transitions {
                println!(
                    "{}\t{}\t{}",
                    t.created_at.format("%Y-%m-%d %H:%M:%S"),
                    t.source.as_str(),
                    t.path.display(),
                );
            }
        }
    }

    Ok(())
}
