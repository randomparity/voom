use std::collections::HashMap;

use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;

use crate::app;
use crate::cli::{OutputFormat, ReportArgs};
use crate::config;
use crate::output;
use crate::stats;
use voom_domain::storage::PlanStorage;

pub fn run(args: ReportArgs) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    if args.plans {
        return run_plans_report(&*store, &args.format);
    }

    let files = store
        .list_files(&voom_domain::FileFilters::default())
        .context("failed to list files from database")?;

    if files.is_empty() {
        if args.format.is_machine() {
            if matches!(args.format, OutputFormat::Json) {
                // Emit the correct empty schema for the requested sub-report
                let empty: serde_json::Value = if args.issues {
                    serde_json::json!([])
                } else {
                    serde_json::json!({
                        "total_files": 0,
                        "total_size": 0,
                        "containers": [],
                        "codecs": [],
                    })
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&empty).expect("report is serializable")
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

    if args.issues {
        return run_issues_report(&files, &args.format);
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
                style(voom_domain::utils::format::format_size(total_size)).cyan(),
                style(voom_domain::utils::format::format_duration(total_duration)).dim(),
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
        OutputFormat::Plain => {
            for file in &files {
                println!("{}", file.path.display());
            }
        }
    }

    Ok(())
}

fn run_issues_report(files: &[voom_domain::MediaFile], format: &OutputFormat) -> Result<()> {
    let files_with_issues: Vec<_> = files
        .iter()
        .filter_map(|f| {
            let violations = f.plugin_metadata.get("safeguard_violations")?;
            let parsed: Vec<voom_domain::SafeguardViolation> =
                serde_json::from_value(violations.clone()).ok()?;
            if parsed.is_empty() {
                return None;
            }
            Some((f, parsed))
        })
        .collect();

    if files_with_issues.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!("{}", style("No files with safeguard violations.").green());
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            let entries: Vec<serde_json::Value> = files_with_issues
                .iter()
                .map(|(f, violations)| {
                    serde_json::json!({
                        "path": f.path.display().to_string(),
                        "violations": violations,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&entries).expect("report is serializable")
            );
        }
        OutputFormat::Table => {
            println!(
                "{} ({} files)",
                style("Safeguard Violations").bold().underlined(),
                files_with_issues.len()
            );
            println!();
            let mut table = output::new_table();
            table.set_header(vec!["Path", "Violation", "Phase", "Message"]);
            for (f, violations) in &files_with_issues {
                let path = f
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                for v in violations {
                    table.add_row(vec![
                        Cell::new(&path),
                        Cell::new(v.kind.as_str()),
                        Cell::new(&v.phase_name),
                        Cell::new(&v.message),
                    ]);
                }
            }
            println!("{table}");
        }
        OutputFormat::Plain => {
            for (f, violations) in &files_with_issues {
                for v in violations {
                    println!("{}\t{}", f.path.display(), v.kind.as_str());
                }
            }
        }
    }

    Ok(())
}

fn run_plans_report(store: &dyn PlanStorage, format: &OutputFormat) -> Result<()> {
    let stats = store
        .plan_stats_by_phase()
        .context("failed to query plan stats")?;

    if stats.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No plan data. Run 'voom process' first.").yellow()
        );
        return Ok(());
    }

    #[derive(Default)]
    struct PhaseAgg {
        completed: u64,
        skipped: u64,
        failed: u64,
        skip_reasons: HashMap<String, u64>,
    }

    let mut phases: Vec<String> = Vec::new();
    let mut by_phase: HashMap<String, PhaseAgg> = HashMap::new();

    for stat in &stats {
        if !by_phase.contains_key(&stat.phase_name) {
            phases.push(stat.phase_name.clone());
        }
        let entry = by_phase.entry(stat.phase_name.clone()).or_default();
        match stat.status {
            voom_domain::storage::PlanStatus::Completed => {
                entry.completed += stat.count;
            }
            voom_domain::storage::PlanStatus::Skipped => {
                entry.skipped += stat.count;
                if let Some(reason) = &stat.skip_reason {
                    *entry.skip_reasons.entry(reason.clone()).or_default() += stat.count;
                }
            }
            voom_domain::storage::PlanStatus::Failed => {
                entry.failed += stat.count;
            }
            _ => {}
        }
    }

    match format {
        OutputFormat::Json => {
            let entries: Vec<serde_json::Value> = phases
                .iter()
                .map(|name| {
                    let ps = by_phase.get(name).expect("phase exists");
                    let mut val = serde_json::json!({
                        "phase": name,
                        "completed": ps.completed,
                        "skipped": ps.skipped,
                        "failed": ps.failed,
                    });
                    if !ps.skip_reasons.is_empty() {
                        val["skip_reasons"] = serde_json::json!(ps.skip_reasons);
                    }
                    val
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&entries).expect("report is serializable")
            );
        }
        OutputFormat::Table => {
            println!("{}", style("Plan Processing Summary").bold().underlined());
            println!();

            let mut table = output::new_table();
            table.set_header(vec![
                "Phase",
                "Completed",
                "Skipped",
                "Failed",
                "Top Skip Reasons",
            ]);
            for name in &phases {
                let ps = by_phase.get(name).expect("phase exists");
                let reasons = output::format_skip_reasons(&ps.skip_reasons, 3);
                table.add_row(vec![
                    Cell::new(name),
                    Cell::new(ps.completed),
                    Cell::new(ps.skipped),
                    Cell::new(ps.failed),
                    Cell::new(&reasons),
                ]);
            }
            println!("{table}");
        }
        OutputFormat::Plain => {
            for name in &phases {
                let ps = by_phase.get(name).expect("phase exists");
                let status = if ps.failed > 0 {
                    "failed"
                } else if ps.completed > 0 {
                    "completed"
                } else {
                    "skipped"
                };
                println!("{name}\t{status}");
            }
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
        // hevc: 2, aac: 2, opus: 1, srt: 1, avc: 1
        assert_eq!(counts[0].1, 2); // either hevc or aac first (both 2)
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
