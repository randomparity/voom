use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;

use voom_domain::media::Container;
use voom_domain::utils::format::{format_duration, format_size};
use voom_domain::FileFilters;

use crate::cli::{FilesCommands, OutputFormat};
use crate::{app, config, output};

pub fn run(cmd: FilesCommands, global_yes: bool) -> Result<()> {
    match cmd {
        FilesCommands::List {
            container,
            codec,
            lang,
            path_prefix,
            limit,
            offset,
            format,
        } => {
            let mut filters = FileFilters::default();
            filters.container = container.as_deref().map(Container::from_extension);
            filters.has_codec = codec;
            filters.has_language = lang;
            filters.path_prefix = path_prefix;
            filters.limit = Some(limit);
            filters.offset = Some(offset);
            list(filters, format)
        }
        FilesCommands::Show { id, format } => show(&id, format),
        FilesCommands::Delete { id, yes } => delete(&id, yes || global_yes),
    }
}

fn list(filters: FileFilters, format: OutputFormat) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let total = store
        .count_files(&filters)
        .context("failed to count files")?;
    let files = store.list_files(&filters).context("failed to list files")?;

    if total == 0 {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                let empty = serde_json::json!({"files": [], "total": 0});
                println!(
                    "{}",
                    serde_json::to_string_pretty(&empty)
                        .expect("serde_json::Value serialization cannot fail")
                );
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No files in database. Run 'voom scan' first.").yellow()
        );
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let response = serde_json::json!({
                "files": files,
                "total": total,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&response)
                    .expect("MediaFile serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            let mut table = output::new_table();
            table.set_header(vec!["Path", "Container", "Size", "Duration", "Tracks"]);

            for file in &files {
                let name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let tracks = compact_track_summary(&file.tracks);
                table.add_row(vec![
                    Cell::new(&name),
                    Cell::new(file.container.as_str()),
                    Cell::new(format_size(file.size)),
                    Cell::new(format_duration(file.duration)),
                    Cell::new(&tracks),
                ]);
            }
            println!("{table}");

            let limit = filters.limit.unwrap_or(100);
            let offset = filters.offset.unwrap_or(0);
            let showing = files.len();
            let total_usize = total as usize;

            if total_usize > showing {
                println!(
                    "Showing {showing} of {total_usize} files \
                     (use --offset {} to see more)",
                    offset + limit,
                );
            } else {
                println!("Showing {showing} of {total_usize} files");
            }
        }
        OutputFormat::Plain => {
            for file in &files {
                println!("{}", file.path.display());
            }
        }
    }

    Ok(())
}

fn show(id: &str, format: OutputFormat) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id).with_context(|| format!("Invalid file ID: {id}"))?;
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let file = store
        .file(&uuid)
        .context("failed to look up file")?
        .ok_or_else(|| anyhow::anyhow!("File not found: {id}"))?;

    match format {
        OutputFormat::Json => output::format_file_json(&file),
        OutputFormat::Table => output::format_file_info(&file, false),
        OutputFormat::Plain => println!("{}", file.path.display()),
    }
    Ok(())
}

fn delete(id: &str, yes: bool) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id).with_context(|| format!("Invalid file ID: {id}"))?;
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    let file = store
        .file(&uuid)
        .context("failed to look up file")?
        .ok_or_else(|| anyhow::anyhow!("File not found: {id}"))?;

    let prompt = format!(
        "Delete {} from database?",
        style(file.path.display()).cyan()
    );
    if !crate::output::confirm(&prompt, yes)? {
        println!("{}", style("Aborted.").dim());
        return Ok(());
    }

    store.delete_file(&uuid).context("failed to delete file")?;

    println!(
        "{} Deleted {} from database",
        style("✓").green(),
        style(file.path.display()).cyan()
    );
    Ok(())
}

/// Produce a compact track summary like "2v 1a 3s".
fn compact_track_summary(tracks: &[voom_domain::media::Track]) -> String {
    let mut video = 0u32;
    let mut audio = 0u32;
    let mut subtitle = 0u32;
    for t in tracks {
        if t.track_type.is_video() {
            video += 1;
        } else if t.track_type.is_audio() {
            audio += 1;
        } else if t.track_type.is_subtitle() {
            subtitle += 1;
        }
    }
    let mut parts = Vec::new();
    if video > 0 {
        parts.push(format!("{video}v"));
    }
    if audio > 0 {
        parts.push(format!("{audio}a"));
    }
    if subtitle > 0 {
        parts.push(format!("{subtitle}s"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use voom_domain::media::{Track, TrackType};

    #[test]
    fn test_compact_track_summary_typical() {
        let tracks = vec![
            Track::new(0, TrackType::Video, "hevc".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
            Track::new(2, TrackType::AudioAlternate, "ac3".into()),
            Track::new(3, TrackType::SubtitleMain, "srt".into()),
            Track::new(4, TrackType::SubtitleMain, "ass".into()),
            Track::new(5, TrackType::SubtitleMain, "srt".into()),
        ];
        assert_eq!(compact_track_summary(&tracks), "1v 2a 3s");
    }

    #[test]
    fn test_compact_track_summary_video_only() {
        let tracks = vec![Track::new(0, TrackType::Video, "hevc".into())];
        assert_eq!(compact_track_summary(&tracks), "1v");
    }

    #[test]
    fn test_compact_track_summary_empty() {
        assert_eq!(compact_track_summary(&[]), "");
    }
}
