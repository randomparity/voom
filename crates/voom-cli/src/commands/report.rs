use std::collections::HashMap;

use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;

use crate::app;
use crate::cli::{OutputFormat, ReportArgs};
use crate::config;
use crate::output;
use crate::stats;
use voom_domain::stats::SnapshotTrigger;
use voom_domain::storage::{FileTransitionStorage, PlanStorage, SnapshotStorage};

pub fn run(args: ReportArgs) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;

    if args.stats {
        return run_stats_report(&*store, &args.format);
    }
    if let Some(n) = args.history {
        return run_history_report(&*store, &args.format, n);
    }

    if args.plans {
        return run_plans_report(&*store, &args.format);
    }

    if args.savings {
        return run_savings_report(&*store, &args.format, args.period.as_deref());
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
        OutputFormat::Json => print_library_json(&files),
        OutputFormat::Table => print_library_table(&files),
        OutputFormat::Plain => {
            for file in &files {
                println!("{}", file.path.display());
            }
        }
    }

    Ok(())
}

fn print_library_json(files: &[voom_domain::MediaFile]) {
    let report = serde_json::json!({
        "total_files": files.len(),
        "total_size": files.iter().map(|f| f.size).sum::<u64>(),
        "containers": stats::container_counts(files),
        "codecs": codec_counts(files),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("report is serializable")
    );
}

fn print_library_table(files: &[voom_domain::MediaFile]) {
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
    let containers = stats::container_counts(files);
    let mut table = output::new_table();
    table.set_header(vec!["Container", "Count"]);
    for (container, count) in &containers {
        table.add_row(vec![Cell::new(container), Cell::new(count)]);
    }
    println!("{table}");
    println!();

    // Codec breakdown
    println!("{}", style("Codecs:").bold());
    let codecs = codec_counts(files);
    let mut table = output::new_table();
    table.set_header(vec!["Codec", "Count"]);
    for (codec, count) in &codecs {
        table.add_row(vec![Cell::new(codec), Cell::new(count)]);
    }
    println!("{table}");
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
        OutputFormat::Json => print_issues_json(&files_with_issues),
        OutputFormat::Table => print_issues_table(&files_with_issues),
        OutputFormat::Plain => print_issues_plain(&files_with_issues),
    }

    Ok(())
}

fn print_issues_json(
    files_with_issues: &[(
        &voom_domain::MediaFile,
        Vec<voom_domain::SafeguardViolation>,
    )],
) {
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

fn print_issues_table(
    files_with_issues: &[(
        &voom_domain::MediaFile,
        Vec<voom_domain::SafeguardViolation>,
    )],
) {
    println!(
        "{} ({} files)",
        style("Safeguard Violations").bold().underlined(),
        files_with_issues.len()
    );
    println!();
    let mut table = output::new_table();
    table.set_header(vec!["Path", "Violation", "Phase", "Message"]);
    for (f, violations) in files_with_issues {
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

fn print_issues_plain(
    files_with_issues: &[(
        &voom_domain::MediaFile,
        Vec<voom_domain::SafeguardViolation>,
    )],
) {
    for (f, violations) in files_with_issues {
        for v in violations {
            println!("{}\t{}", f.path.display(), v.kind.as_str());
        }
    }
}

#[derive(Default)]
struct PhaseAgg {
    completed: u64,
    skipped: u64,
    failed: u64,
    skip_reasons: HashMap<String, u64>,
}

/// Aggregate raw plan stats into per-phase summaries, preserving insertion order.
fn aggregate_plan_stats(
    stats: &[voom_domain::storage::PlanPhaseStat],
) -> (Vec<String>, HashMap<String, PhaseAgg>) {
    let mut phases: Vec<String> = Vec::new();
    let mut by_phase: HashMap<String, PhaseAgg> = HashMap::new();

    for stat in stats {
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

    (phases, by_phase)
}

fn print_plans_json(phases: &[String], by_phase: &HashMap<String, PhaseAgg>) {
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

fn print_plans_table(phases: &[String], by_phase: &HashMap<String, PhaseAgg>) {
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
    for name in phases {
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

fn print_plans_plain(phases: &[String], by_phase: &HashMap<String, PhaseAgg>) {
    for name in phases {
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

    let (phases, by_phase) = aggregate_plan_stats(&stats);

    match format {
        OutputFormat::Json => print_plans_json(&phases, &by_phase),
        OutputFormat::Table => print_plans_table(&phases, &by_phase),
        OutputFormat::Plain => print_plans_plain(&phases, &by_phase),
    }

    Ok(())
}

fn run_stats_report(store: &dyn SnapshotStorage, format: &OutputFormat) -> Result<()> {
    let snapshot = store
        .gather_library_stats(SnapshotTrigger::Manual)
        .context("failed to gather library statistics")?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&snapshot).context("failed to serialize snapshot")?
            );
        }
        OutputFormat::Table => print_stats_table(&snapshot),
        OutputFormat::Plain => print_stats_plain(&snapshot),
    }

    Ok(())
}

fn print_stats_table(snapshot: &voom_domain::stats::LibrarySnapshot) {
    use voom_domain::utils::format::{format_duration, format_size};

    let f = &snapshot.files;
    println!("{}", style("Library Overview").bold().underlined());
    println!(
        "  {} files, {}, {}",
        style(f.total_count).bold(),
        style(format_size(f.total_size_bytes)).cyan(),
        style(format_duration(f.total_duration_secs)).dim(),
    );
    println!(
        "  Avg size: {}  Max: {}  Min: {}",
        style(format_size(f.avg_size_bytes)).dim(),
        style(format_size(f.max_size_bytes)).dim(),
        style(format_size(f.min_size_bytes)).dim(),
    );
    println!();

    print_pair_table("Containers", "Container", "Count", &f.container_counts);

    let v = &snapshot.video;
    print_pair_table("Video Codecs", "Codec", "Count", &v.codec_counts);
    print_pair_table(
        "Video Resolutions",
        "Resolution",
        "Count",
        &v.resolution_counts,
    );
    println!(
        "  HDR: {}  VFR: {}",
        style(v.hdr_count).bold(),
        style(v.vfr_count).bold(),
    );
    println!();

    let a = &snapshot.audio;
    print_pair_table("Audio Types", "Type", "Count", &a.type_counts);
    let top_langs: Vec<_> = a.language_counts.iter().take(20).cloned().collect();
    print_pair_table("Audio Languages (top 20)", "Language", "Count", &top_langs);
    print_pair_table("Audio Codecs", "Codec", "Count", &a.codec_counts);

    let s = &snapshot.subtitles;
    print_pair_table(
        "Subtitles by Language",
        "Language",
        "Count",
        &s.language_counts,
    );

    let p = &snapshot.processing;
    println!("{}", style("Processing").bold().underlined());
    if !p.plans_by_status.is_empty() {
        let mut table = output::new_table();
        table.set_header(vec!["Status", "Count"]);
        for (status, count) in &p.plans_by_status {
            table.add_row(vec![Cell::new(status), Cell::new(count)]);
        }
        println!("{table}");
    }
    let size_label = if p.total_size_saved_bytes >= 0 {
        format!("{} saved", format_size(p.total_size_saved_bytes as u64))
    } else {
        format!(
            "{} added",
            format_size(p.total_size_saved_bytes.unsigned_abs())
        )
    };
    println!(
        "  Total time: {}  Size change: {}",
        style(format_duration(p.total_processing_time_ms as f64 / 1000.0)).dim(),
        style(size_label).dim(),
    );
    println!();

    let j = &snapshot.jobs;
    if !j.by_status.is_empty() {
        print_pair_table("Jobs", "Status", "Count", &j.by_status);
    }
}

fn print_stats_plain(snapshot: &voom_domain::stats::LibrarySnapshot) {
    let f = &snapshot.files;
    println!("total_files={}", f.total_count);
    println!("total_size={}", f.total_size_bytes);
    println!("total_duration_secs={:.1}", f.total_duration_secs);
    println!("avg_size={}", f.avg_size_bytes);
    println!("max_size={}", f.max_size_bytes);
    println!("min_size={}", f.min_size_bytes);
    println!("hdr_count={}", snapshot.video.hdr_count);
    println!("vfr_count={}", snapshot.video.vfr_count);
    for (name, count) in &f.container_counts {
        println!("container_{name}={count}");
    }
    for (name, count) in &snapshot.video.codec_counts {
        println!("video_codec_{name}={count}");
    }
    for (name, count) in &snapshot.audio.codec_counts {
        println!("audio_codec_{name}={count}");
    }
    for (name, count) in &snapshot.subtitles.language_counts {
        println!("subtitle_lang_{name}={count}");
    }
    for (status, count) in &snapshot.processing.plans_by_status {
        println!("plan_status_{status}={count}");
    }
    for (status, count) in &snapshot.jobs.by_status {
        println!("job_status_{status}={count}");
    }
}

fn print_pair_table(title: &str, col1: &str, col2: &str, data: &[(String, u64)]) {
    if data.is_empty() {
        return;
    }
    println!("{}", style(title).bold());
    let mut table = output::new_table();
    table.set_header(vec![col1, col2]);
    for (key, count) in data {
        table.add_row(vec![Cell::new(key), Cell::new(count)]);
    }
    println!("{table}");
    println!();
}

fn run_history_report(
    store: &dyn SnapshotStorage,
    format: &OutputFormat,
    limit: u32,
) -> Result<()> {
    let snapshots = store
        .list_snapshots(limit)
        .context("failed to list snapshots")?;

    if snapshots.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!("[]");
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No snapshots yet. Run 'voom scan' or 'voom report --stats' first.").yellow()
        );
        return Ok(());
    }

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&snapshots)
                    .context("failed to serialize snapshots")?
            );
        }
        OutputFormat::Table => {
            use voom_domain::utils::format::format_size;

            println!("{}", style("Snapshot History").bold().underlined());
            println!();
            let mut table = output::new_table();
            table.set_header(vec![
                "Timestamp",
                "Trigger",
                "Files",
                "Total Size",
                "Duration",
                "HDR",
                "VFR",
            ]);
            for snap in &snapshots {
                table.add_row(vec![
                    Cell::new(snap.captured_at.format("%Y-%m-%d %H:%M:%S")),
                    Cell::new(snap.trigger.as_str()),
                    Cell::new(snap.files.total_count),
                    Cell::new(format_size(snap.files.total_size_bytes)),
                    Cell::new(voom_domain::utils::format::format_duration(
                        snap.files.total_duration_secs,
                    )),
                    Cell::new(snap.video.hdr_count),
                    Cell::new(snap.video.vfr_count),
                ]);
            }
            println!("{table}");
        }
        OutputFormat::Plain => {
            for snap in &snapshots {
                println!(
                    "{}\t{}\t{}\t{}\t{:.0}\t{}\t{}",
                    snap.captured_at.format("%Y-%m-%d %H:%M:%S"),
                    snap.trigger.as_str(),
                    snap.files.total_count,
                    snap.files.total_size_bytes,
                    snap.files.total_duration_secs,
                    snap.video.hdr_count,
                    snap.video.vfr_count,
                );
            }
        }
    }

    Ok(())
}

fn run_savings_report(
    store: &dyn FileTransitionStorage,
    format: &OutputFormat,
    period_str: Option<&str>,
) -> Result<()> {
    let period = match period_str {
        Some(s) => {
            let p = voom_domain::stats::TimePeriod::parse(s).context(format!(
                "invalid period '{s}': expected day, week, or month"
            ))?;
            Some(p)
        }
        None => None,
    };

    let report = store
        .savings_by_provenance(period)
        .context("failed to query savings report")?;

    if report.buckets.is_empty() {
        if format.is_machine() {
            if matches!(format, OutputFormat::Json) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("report is serializable")
                );
            }
            return Ok(());
        }
        eprintln!(
            "{}",
            style("No savings data. Run 'voom process' first.").yellow()
        );
        return Ok(());
    }

    match format {
        OutputFormat::Json => print_savings_json(&report),
        OutputFormat::Table => print_savings_table(&report, period.is_some()),
        OutputFormat::Plain => print_savings_plain(&report, period.is_some()),
    }

    Ok(())
}

fn print_savings_json(report: &voom_domain::stats::SavingsReport) {
    println!(
        "{}",
        serde_json::to_string_pretty(report).expect("report is serializable")
    );
}

fn print_savings_table(report: &voom_domain::stats::SavingsReport, show_period: bool) {
    use voom_domain::utils::format::{format_duration, format_size};

    println!(
        "{}",
        style("Space Savings by Provenance").bold().underlined()
    );
    println!();

    let mut table = output::new_table();
    let mut headers: Vec<&str> = vec!["Executor", "Phase"];
    if show_period {
        headers.push("Period");
    }
    headers.extend_from_slice(&["Files", "Transitions", "Saved", "Time"]);
    table.set_header(headers);

    for b in &report.buckets {
        let saved_label = if b.bytes_saved >= 0 {
            format_size(b.bytes_saved as u64)
        } else {
            format!("+{}", format_size(b.bytes_saved.unsigned_abs()))
        };

        let mut row: Vec<Cell> = vec![
            Cell::new(b.executor.as_deref().unwrap_or("-")),
            Cell::new(b.phase.as_deref().unwrap_or("-")),
        ];
        if show_period {
            row.push(Cell::new(b.period.as_deref().unwrap_or("-")));
        }
        row.extend_from_slice(&[
            Cell::new(b.file_count),
            Cell::new(b.transition_count),
            Cell::new(&saved_label),
            Cell::new(format_duration(b.duration_ms as f64 / 1000.0)),
        ]);
        table.add_row(row);
    }
    println!("{table}");

    let total_label = if report.total_bytes_saved >= 0 {
        format!("{} saved", format_size(report.total_bytes_saved as u64))
    } else {
        format!(
            "{} added",
            format_size(report.total_bytes_saved.unsigned_abs())
        )
    };
    println!(
        "\n  Total: {} across {} transitions",
        style(total_label).bold(),
        style(report.total_transitions).bold(),
    );
}

fn print_savings_plain(report: &voom_domain::stats::SavingsReport, show_period: bool) {
    for b in &report.buckets {
        let executor = b.executor.as_deref().unwrap_or("-");
        let phase = b.phase.as_deref().unwrap_or("-");
        if show_period {
            let period = b.period.as_deref().unwrap_or("-");
            println!(
                "{executor}\t{phase}\t{period}\t{}\t{}\t{}",
                b.file_count, b.transition_count, b.bytes_saved,
            );
        } else {
            println!(
                "{executor}\t{phase}\t{}\t{}\t{}",
                b.file_count, b.transition_count, b.bytes_saved,
            );
        }
    }
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
