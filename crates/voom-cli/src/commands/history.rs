use anyhow::Result;
use console::style;
use uuid::Uuid;
use voom_domain::storage::{FileStorage, FileTransitionStorage};
use voom_domain::transition::FileTransition;

use crate::app;
use crate::cli::{HistoryArgs, OutputFormat};
use crate::output;

/// Maximum predecessors to walk (prevents infinite loops from corrupt data).
const MAX_PREDECESSORS: usize = 50;

/// Walk the `superseded_by` chain backward from `start_id`, collecting all
/// file IDs in lineage order (oldest first, current last).
pub(crate) fn collect_lineage(store: &dyn FileStorage, start_id: Uuid) -> Vec<Uuid> {
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
pub(crate) fn collect_lineage_transitions(
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
pub(crate) fn format_snapshot_cell(snap: &voom_domain::snapshot::MetadataSnapshot) -> String {
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
                .map(|t| serde_json::to_value(t).expect("FileTransition serialization cannot fail"))
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
            let table = output::render_transitions_table(&transitions);
            println!("{table}");
        }
        OutputFormat::Plain | OutputFormat::Csv => {
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
