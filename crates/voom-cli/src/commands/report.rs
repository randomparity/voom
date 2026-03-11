use anyhow::Result;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, ContentArrangement, Table};
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::{OutputFormat, ReportArgs};

pub async fn run(args: ReportArgs) -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    use voom_domain::storage::StorageTrait;
    let files = store
        .list_files(&voom_domain::FileFilters::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if files.is_empty() {
        println!(
            "{}",
            "No files in database. Run 'voom scan' first.".yellow()
        );
        return Ok(());
    }

    match args.format {
        OutputFormat::Json => {
            let report = serde_json::json!({
                "total_files": files.len(),
                "total_size": files.iter().map(|f| f.size).sum::<u64>(),
                "containers": container_counts(&files),
                "codecs": codec_counts(&files),
            });
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
        }
        OutputFormat::Table => {
            println!("{}", "Library Report".bold().underline());
            println!();

            let total_size: u64 = files.iter().map(|f| f.size).sum();
            let total_duration: f64 = files.iter().map(|f| f.duration).sum();

            println!(
                "  {} files, {}, {}",
                files.len().to_string().bold(),
                voom_domain::utils::datetime::format_size(total_size).cyan(),
                voom_domain::utils::datetime::format_duration(total_duration).dimmed(),
            );
            println!();

            // Container breakdown
            println!("{}", "Containers:".bold());
            let containers = container_counts(&files);
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic);
            table.set_header(vec!["Container", "Count"]);
            for (container, count) in &containers {
                table.add_row(vec![Cell::new(container), Cell::new(count)]);
            }
            println!("{table}");
            println!();

            // Codec breakdown
            println!("{}", "Codecs:".bold());
            let codecs = codec_counts(&files);
            let mut table = Table::new();
            table
                .load_preset(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic);
            table.set_header(vec!["Codec", "Count"]);
            for (codec, count) in &codecs {
                table.add_row(vec![Cell::new(codec), Cell::new(count)]);
            }
            println!("{table}");
        }
    }

    Ok(())
}

fn container_counts(files: &[voom_domain::MediaFile]) -> Vec<(String, usize)> {
    let mut counts = std::collections::HashMap::new();
    for file in files {
        *counts
            .entry(file.container.as_str().to_string())
            .or_insert(0) += 1;
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

fn codec_counts(files: &[voom_domain::MediaFile]) -> Vec<(String, usize)> {
    let mut counts = std::collections::HashMap::new();
    for file in files {
        for track in &file.tracks {
            *counts.entry(track.codec.clone()).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}
