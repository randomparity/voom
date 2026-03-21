use anyhow::Result;
use comfy_table::Cell;
use console::style;

use crate::app;
use crate::cli::{OutputFormat, ReportArgs};
use crate::output;
use crate::stats;

pub fn run(args: ReportArgs) -> Result<()> {
    let config = app::load_config()?;
    let store = app::open_store(&config)?;

    let files = store
        .list_files(&voom_domain::FileFilters::default())
        .map_err(|e| anyhow::anyhow!("failed to list files from database: {e}"))?;

    if files.is_empty() {
        println!(
            "{}",
            style("No files in database. Run 'voom scan' first.").yellow()
        );
        return Ok(());
    }

    match args.format {
        OutputFormat::Json => {
            let report = serde_json::json!({
                "total_files": files.len(),
                "total_size": files.iter().map(|f| f.size).sum::<u64>(),
                "containers": stats::container_counts(&files),
                "codecs": codec_counts(&files),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("report is serializable")
            );
        }
        OutputFormat::Table => {
            println!("{}", style("Library Report").bold().underlined());
            println!();

            let total_size: u64 = files.iter().map(|f| f.size).sum();
            let total_duration: f64 = files.iter().map(|f| f.duration).sum();

            println!(
                "  {} files, {}, {}",
                style(files.len()).bold(),
                style(voom_domain::utils::datetime::format_size(total_size)).cyan(),
                style(voom_domain::utils::datetime::format_duration(
                    total_duration
                ))
                .dim(),
            );
            println!();

            // Container breakdown
            println!("{}", style("Containers:").bold());
            let containers = stats::container_counts(&files);
            let mut table = output::new_table();
            table.set_header(vec!["Container", "Count"]);
            for (container, count) in &containers {
                table.add_row(vec![Cell::new(container), Cell::new(count)]);
            }
            println!("{table}");
            println!();

            // Codec breakdown
            println!("{}", style("Codecs:").bold());
            let codecs = codec_counts(&files);
            let mut table = output::new_table();
            table.set_header(vec!["Codec", "Count"]);
            for (codec, count) in &codecs {
                table.add_row(vec![Cell::new(codec), Cell::new(count)]);
            }
            println!("{table}");
        }
    }

    Ok(())
}

fn codec_counts(files: &[voom_domain::MediaFile]) -> Vec<(String, usize)> {
    stats::count_by(files, |f| {
        f.tracks.iter().map(|t| t.codec.clone()).collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::media::{MediaFile, Track, TrackType};

    fn make_track(codec: &str) -> Track {
        Track::new(0, TrackType::Video, codec.to_string())
    }

    fn make_file(codecs: &[&str]) -> MediaFile {
        MediaFile::new(PathBuf::from("/test.mkv"))
            .with_tracks(codecs.iter().map(|c| make_track(c)).collect())
    }

    #[test]
    fn codec_counts_empty_files() {
        let result = codec_counts(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn codec_counts_single_file() {
        let files = vec![make_file(&["hevc", "aac", "aac"])];
        let counts = codec_counts(&files);
        assert_eq!(counts[0], ("aac".to_string(), 2));
        assert_eq!(counts[1], ("hevc".to_string(), 1));
    }

    #[test]
    fn codec_counts_multiple_files() {
        let files = vec![
            make_file(&["hevc", "aac"]),
            make_file(&["hevc", "opus", "srt"]),
            make_file(&["avc", "aac"]),
        ];
        let counts = codec_counts(&files);
        // hevc: 2, aac: 2, opus: 1, srt: 1, avc: 1
        assert_eq!(counts[0].1, 2); // either hevc or aac first (both 2)
        assert_eq!(counts[1].1, 2);
    }

    #[test]
    fn codec_counts_sorted_descending() {
        let files = vec![make_file(&["a", "b", "b", "b", "c", "c"])];
        let counts = codec_counts(&files);
        assert_eq!(counts[0], ("b".to_string(), 3));
        assert_eq!(counts[1], ("c".to_string(), 2));
        assert_eq!(counts[2], ("a".to_string(), 1));
    }
}
