use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;

use crate::cli::OutputFormat;
use crate::output;

// ── File list ───────────────────────────────────────────────

pub(super) fn run(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let files = store
        .list_files(&voom_domain::storage::FileFilters::default())
        .context("failed to list files from database")?;

    if files.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
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
        OutputFormat::Json => print_file_list_json(&files),
        OutputFormat::Table => print_file_list_table(&files),
        OutputFormat::Plain => {
            for file in &files {
                println!("{}", file.path.display());
            }
        }
        OutputFormat::Csv => print_file_list_csv(&files)?,
    }

    Ok(())
}

fn print_file_list_json(files: &[voom_domain::MediaFile]) {
    let report = serde_json::json!({
        "total_files": files.len(),
        "total_size": files.iter().map(|f| f.size).sum::<u64>(),
        "containers": container_counts(files),
        "codecs": codec_counts(files),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report is serializable")
    );
}

fn print_file_list_table(files: &[voom_domain::MediaFile]) {
    println!("{}", style("Library Report").bold().underlined());
    println!();

    let total_size: u64 = files.iter().map(|f| f.size).sum();
    let total_duration: f64 = files.iter().map(|f| f.duration).sum();

    println!(
        "  {} files, {}, {}",
        style(files.len()).bold(),
        style(voom_domain::utils::format::format_size(total_size)).cyan(),
        style(voom_domain::utils::format::format_duration(total_duration)).dim(),
    );
    println!();

    println!("{}", style("Containers:").bold());
    let containers = container_counts(files);
    let mut table = output::new_table();
    table.set_header(vec!["Container", "Count"]);
    for (container, count) in &containers {
        table.add_row(vec![Cell::new(container), Cell::new(count)]);
    }
    println!("{table}");
    println!();

    println!("{}", style("Codecs:").bold());
    let codecs = codec_counts(files);
    let mut table = output::new_table();
    table.set_header(vec!["Codec", "Count"]);
    for (codec, count) in &codecs {
        table.add_row(vec![Cell::new(codec), Cell::new(count)]);
    }
    println!("{table}");
}

fn print_file_list_csv(files: &[voom_domain::MediaFile]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["path", "size", "duration", "container", "codec"])?;
    for f in files {
        let primary_codec = f.tracks.first().map_or("", |t| t.codec.as_str());
        wtr.write_record([
            &f.path.display().to_string(),
            &f.size.to_string(),
            &format!("{:.1}", f.duration),
            f.container.as_str(),
            primary_codec,
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

fn count_by<T, I, F>(items: &[T], key_fn: F) -> Vec<(String, usize)>
where
    F: Fn(&T) -> I,
    I: IntoIterator<Item = String>,
{
    let mut counts = std::collections::HashMap::new();
    for item in items {
        for key in key_fn(item) {
            *counts.entry(key).or_insert(0usize) += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

fn container_counts(files: &[voom_domain::MediaFile]) -> Vec<(String, usize)> {
    count_by(files, |f| std::iter::once(f.container.as_str().to_string()))
}

fn codec_counts(files: &[voom_domain::MediaFile]) -> Vec<(String, usize)> {
    count_by(files, |f| {
        f.tracks.iter().map(|t| t.codec.clone()).collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use voom_domain::media::{MediaFile, Track, TrackType};

    use super::*;

    fn make_track(codec: &str) -> Track {
        Track::new(0, TrackType::Video, codec.to_string())
    }

    fn make_file(codecs: &[&str]) -> MediaFile {
        MediaFile::new(PathBuf::from("/test.mkv"))
            .with_tracks(codecs.iter().map(|c| make_track(c)).collect())
    }

    #[test]
    fn test_codec_counts_empty_files() {
        let result = codec_counts(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_codec_counts_single_file() {
        let files = vec![make_file(&["hevc", "aac", "aac"])];
        let counts = codec_counts(&files);
        assert_eq!(counts[0], ("aac".to_string(), 2));
        assert_eq!(counts[1], ("hevc".to_string(), 1));
    }

    #[test]
    fn test_codec_counts_multiple_files() {
        let files = vec![
            make_file(&["hevc", "aac"]),
            make_file(&["hevc", "opus", "srt"]),
            make_file(&["avc", "aac"]),
        ];
        let counts = codec_counts(&files);
        assert_eq!(counts[0].1, 2);
        assert_eq!(counts[1].1, 2);
    }

    #[test]
    fn test_codec_counts_sorted_descending() {
        let files = vec![make_file(&["a", "b", "b", "b", "c", "c"])];
        let counts = codec_counts(&files);
        assert_eq!(counts[0], ("b".to_string(), 3));
        assert_eq!(counts[1], ("c".to_string(), 2));
        assert_eq!(counts[2], ("a".to_string(), 1));
    }
}
