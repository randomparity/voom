use anyhow::Result;
use console::style;
use uuid::Uuid;
use voom_domain::storage::{FileStorage, FileTransitionStorage};
use voom_domain::transition::{FileTransition, TransitionSource};
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
    let mut seen = std::collections::HashSet::from([start_id]);
    let mut current = start_id;

    loop {
        if chain.len() > MAX_PREDECESSORS {
            tracing::warn!(
                "predecessor chain exceeded {MAX_PREDECESSORS} entries, \
                 truncating"
            );
            break;
        }

        match store.predecessor_id_of(&current) {
            Ok(Some(pred_id)) => {
                if !seen.insert(pred_id) {
                    tracing::warn!("cycle detected in predecessor chain at {}", pred_id);
                    break;
                }
                chain.push(pred_id);
                current = pred_id;
            }
            Ok(None) => break,
            Err(e) => {
                tracing::warn!("failed to walk predecessor chain: {e}");
                break;
            }
        }
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

/// Format a metadata snapshot as a compact table cell string.
fn format_snapshot_cell(snap: &voom_domain::snapshot::MetadataSnapshot) -> String {
    let mut parts = Vec::new();

    if let Some(ref res) = snap.resolution {
        parts.push(res.clone());
    }

    let mut track_counts = Vec::new();
    for &(count, label) in &[
        (snap.video_tracks, "v"),
        (snap.audio_tracks, "a"),
        (snap.subtitle_tracks, "s"),
    ] {
        if count > 0 {
            track_counts.push(format!("{count}{label}"));
        }
    }

    if !track_counts.is_empty() {
        parts.push(track_counts.join("+"));
    }

    if parts.is_empty() {
        "\u{2014}".to_string()
    } else {
        parts.join(" ")
    }
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
                        "duration_ms": t.duration_ms,
                        "actions_taken": t.actions_taken,
                        "tracks_modified": t.tracks_modified,
                        "outcome": t.outcome.map(|o| o.as_str()),
                        "policy_name": &t.policy_name,
                        "phase_name": &t.phase_name,
                        "metadata_snapshot": t.metadata_snapshot.as_ref()
                            .and_then(|s| serde_json::to_value(s).ok()),
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
            let col_count = if has_lineage { 8 } else { 7 };
            if has_lineage {
                table.set_header(vec![
                    "#",
                    "Date",
                    "Source",
                    "File ID",
                    "Size",
                    "From Hash",
                    "To Hash",
                    "Media",
                ]);
            } else {
                table.set_header(vec![
                    "#",
                    "Date",
                    "Source",
                    "Size",
                    "From Hash",
                    "To Hash",
                    "Media",
                ]);
            }

            let mut prev_file_id: Option<Uuid> = None;

            for (i, t) in transitions.iter().enumerate() {
                if has_lineage {
                    if let Some(prev) = prev_file_id {
                        if prev != t.file_id {
                            let sep = style("── external modification ──").dim().to_string();
                            let mut sep_row: Vec<comfy_table::Cell> =
                                vec![comfy_table::Cell::new(""), comfy_table::Cell::new(&sep)];
                            for _ in 2..col_count {
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

                let source_display = match (&t.source, &t.phase_name, &t.outcome) {
                    (TransitionSource::Voom, Some(phase), Some(outcome)) => {
                        format!("voom:{phase} ({})", outcome.as_str())
                    }
                    _ => t.source.as_str().to_string(),
                };

                let size_cell = match t.from_size {
                    Some(from_sz) => {
                        let delta = t.to_size as i64 - from_sz as i64;
                        let formatted = format::format_size(t.to_size);
                        if delta == 0 {
                            formatted
                        } else if delta < 0 {
                            format!("{formatted} (-{})", format::format_size((-delta) as u64))
                        } else {
                            format!("{formatted} (+{})", format::format_size(delta as u64))
                        }
                    }
                    None => format::format_size(t.to_size),
                };

                let mut row = vec![
                    comfy_table::Cell::new(i + 1),
                    comfy_table::Cell::new(date),
                    comfy_table::Cell::new(source_display),
                ];

                if has_lineage {
                    let short_id = &t.file_id.to_string()[..8];
                    row.push(comfy_table::Cell::new(format!("{short_id}...")));
                }

                row.push(comfy_table::Cell::new(size_cell));
                row.push(comfy_table::Cell::new(from));
                row.push(comfy_table::Cell::new(to));

                let media_cell = t
                    .metadata_snapshot
                    .as_ref()
                    .map(format_snapshot_cell)
                    .unwrap_or_else(|| "\u{2014}".to_string());
                row.push(comfy_table::Cell::new(media_cell));

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

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::snapshot::MetadataSnapshot;

    fn make_snapshot(
        video: u32,
        audio: u32,
        subtitle: u32,
        resolution: Option<&str>,
    ) -> MetadataSnapshot {
        serde_json::from_value(serde_json::json!({
            "container": "mkv",
            "video_tracks": video,
            "audio_tracks": audio,
            "subtitle_tracks": subtitle,
            "codecs": [],
            "resolution": resolution,
            "duration_secs": 0.0,
        }))
        .expect("valid snapshot JSON")
    }

    #[test]
    fn format_snapshot_typical() {
        let snap = make_snapshot(1, 3, 2, Some("3840x2160"));
        assert_eq!(format_snapshot_cell(&snap), "3840x2160 1v+3a+2s");
    }

    #[test]
    fn format_snapshot_no_resolution() {
        let snap = make_snapshot(0, 2, 0, None);
        assert_eq!(format_snapshot_cell(&snap), "2a");
    }

    #[test]
    fn format_snapshot_empty() {
        let snap = make_snapshot(0, 0, 0, None);
        assert_eq!(format_snapshot_cell(&snap), "\u{2014}");
    }

    #[test]
    fn format_snapshot_video_only() {
        let snap = make_snapshot(1, 0, 0, Some("1920x1080"));
        assert_eq!(format_snapshot_cell(&snap), "1920x1080 1v");
    }
}
