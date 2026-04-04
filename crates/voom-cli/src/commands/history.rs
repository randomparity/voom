use anyhow::Result;
use console::style;
use uuid::Uuid;
use voom_domain::storage::{FileStorage, FileTransitionStorage};
use voom_domain::transition::FileTransition;
use voom_domain::utils::format;

use crate::app;
use crate::cli::{HistoryArgs, OutputFormat};
use crate::output;

/// Maximum predecessors to walk (prevents infinite loops from corrupt data).
const MAX_PREDECESSORS: usize = 50;

/// Walk the superseded_by chain backward from `start_id`, collecting all
/// file IDs in lineage order (oldest first, current last).
fn collect_lineage(store: &dyn FileStorage, start_id: Uuid) -> Vec<Uuid> {
    let mut chain = vec![start_id];
    let mut current = start_id;

    for _ in 0..MAX_PREDECESSORS {
        match store.predecessor_of(&current) {
            Ok(Some(pred)) => {
                chain.push(pred.id);
                current = pred.id;
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("failed to walk predecessor chain: {e}");
                break;
            }
        }
    }

    if chain.len() > MAX_PREDECESSORS {
        tracing::warn!("predecessor chain exceeded {MAX_PREDECESSORS} entries, truncating");
    }

    chain.reverse(); // oldest first
    chain
}

/// Collect transitions for an entire lineage chain.
fn collect_lineage_transitions(
    store: &dyn FileTransitionStorage,
    lineage: &[Uuid],
) -> Vec<FileTransition> {
    let mut all_transitions = Vec::new();
    for file_id in lineage {
        match store.transitions_for_file(file_id) {
            Ok(transitions) => all_transitions.extend(transitions),
            Err(e) => {
                tracing::warn!("failed to load transitions for {file_id}: {e}");
            }
        }
    }
    all_transitions
}

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
        Some(file) => {
            let lineage = collect_lineage(store.as_ref(), file.id);
            collect_lineage_transitions(store.as_ref(), &lineage)
        }
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
            let has_lineage = transitions.len() > 1
                && transitions.windows(2).any(|w| w[0].file_id != w[1].file_id);

            println!(
                "{} for {}:\n",
                style(format!("{} transition entries", transitions.len())).bold(),
                style(path.display()).cyan()
            );

            let mut table = output::new_table();
            if has_lineage {
                table.set_header(vec![
                    "#",
                    "Date",
                    "Source",
                    "File ID",
                    "From Hash",
                    "To Hash",
                ]);
            } else {
                table.set_header(vec!["#", "Date", "Source", "From Hash", "To Hash"]);
            }

            let mut prev_file_id: Option<Uuid> = None;

            for (i, t) in transitions.iter().enumerate() {
                if has_lineage {
                    if let Some(prev) = prev_file_id {
                        if prev != t.file_id {
                            let sep = style("── external modification ──").dim().to_string();
                            let mut sep_row: Vec<comfy_table::Cell> =
                                vec![comfy_table::Cell::new(""), comfy_table::Cell::new(&sep)];
                            for _ in 2..6 {
                                sep_row.push(comfy_table::Cell::new(""));
                            }
                            table.add_row(sep_row);
                        }
                    }
                    prev_file_id = Some(t.file_id);
                }

                let date = format::format_display(&t.created_at);
                let from = t
                    .from_hash
                    .as_deref()
                    .map(output::hash_preview)
                    .unwrap_or("—");
                let to = output::hash_preview(&t.to_hash);

                let mut row = vec![
                    comfy_table::Cell::new(i + 1),
                    comfy_table::Cell::new(date),
                    comfy_table::Cell::new(t.source.as_str()),
                ];

                if has_lineage {
                    let short_id = &t.file_id.to_string()[..8];
                    row.push(comfy_table::Cell::new(format!("{short_id}...")));
                }

                row.push(comfy_table::Cell::new(from));
                row.push(comfy_table::Cell::new(to));

                table.add_row(row);
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
