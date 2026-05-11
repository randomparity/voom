use std::collections::HashMap;
use std::io::Write as _;

use anyhow::{Context, Result};
use comfy_table::Cell;
use console::style;
use serde::Serialize;

use crate::app;
use crate::cli::{OutputFormat, ReportArgs};
use crate::config;
use crate::output;
use voom_domain::stats::{LibrarySnapshot, SavingsReport, TimePeriod};
use voom_domain::storage::{PlanPhaseStat, TranscodeOutcomeFilters};
use voom_domain::transcode::TranscodeOutcome;
use voom_domain::verification::IntegritySummary;
use voom_report::{
    DatabaseStats, IssueReport, ReportPlugin, ReportRequest, ReportResult, ReportSection,
};

pub fn run(args: &ReportArgs) -> Result<()> {
    let config = config::load_config()?;
    let store = app::open_store(&config)?;
    let format = report_format(args);

    if args.errors {
        return run_errors(&*store, args, format);
    }

    if args.snapshot {
        let snapshot =
            ReportPlugin::capture_snapshot(&*store, voom_domain::stats::SnapshotTrigger::Manual)?;
        format_snapshot(&snapshot, format);
        return Ok(());
    }

    if args.files {
        return run_file_list(&*store, format);
    }

    if args.integrity && args.vmaf {
        return run_integrity_and_vmaf(&*store, format);
    }

    if args.integrity {
        return run_integrity(&*store, format);
    }

    if args.vmaf {
        return run_vmaf(&*store, format);
    }

    if args.loudness {
        return run_loudness(&*store, format);
    }

    let request = build_request(args)?;
    let result = ReportPlugin::query(&*store, &request)?;

    if is_summary_request(args) {
        format_summary(&result, format)?;
    } else {
        format_result(&result, format)?;
        if args.database && !format.is_machine() {
            print_retention_footer(&*store);
        }
    }
    Ok(())
}

fn report_format(args: &ReportArgs) -> OutputFormat {
    args.format
}

fn build_request(args: &ReportArgs) -> Result<ReportRequest> {
    if args.all {
        let mut req = ReportRequest::all();
        if let Some(ref p) = args.period {
            let period = TimePeriod::parse(p).context(format!(
                "invalid period '{p}': expected day, week, or month"
            ))?;
            req = req.with_period(period);
        }
        if let Some(n) = args.history {
            req = req.with_history_limit(n);
        }
        return Ok(req);
    }

    let mut sections = Vec::new();

    if args.library {
        sections.push(ReportSection::Library);
    }
    if args.plans {
        sections.push(ReportSection::Plans);
    }
    if args.savings {
        sections.push(ReportSection::Savings);
    }
    if args.history.is_some() {
        sections.push(ReportSection::History);
    }
    if args.issues {
        sections.push(ReportSection::Issues);
    }
    if args.database {
        sections.push(ReportSection::Database);
    }

    if sections.is_empty() {
        return Ok(ReportRequest::summary());
    }

    let mut req = ReportRequest::new(sections);
    if let Some(ref p) = args.period {
        let period = TimePeriod::parse(p).context(format!(
            "invalid period '{p}': expected day, week, or month"
        ))?;
        req = req.with_period(period);
    }
    if let Some(n) = args.history {
        req = req.with_history_limit(n);
    }
    Ok(req)
}

fn is_summary_request(args: &ReportArgs) -> bool {
    !args.all
        && !args.library
        && !args.plans
        && !args.savings
        && !args.issues
        && !args.database
        && !args.errors
        && !args.vmaf
        && !args.loudness
        && args.history.is_none()
}

fn format_snapshot(snapshot: &LibrarySnapshot, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(snapshot).expect("snapshot is serializable")
            );
        }
        OutputFormat::Table => {
            println!(
                "{} Snapshot captured: {} files, {}",
                style("OK").green().bold(),
                snapshot.files.total_count,
                voom_domain::utils::format::format_size(snapshot.files.total_size_bytes),
            );
        }
        OutputFormat::Plain | OutputFormat::Csv => {
            println!(
                "snapshot\t{}\t{}",
                snapshot.files.total_count, snapshot.files.total_size_bytes,
            );
        }
    }
}

fn format_summary(result: &ReportResult, format: OutputFormat) -> Result<()> {
    let Some(ref snapshot) = result.library else {
        if !format.is_machine() {
            eprintln!(
                "{}",
                style("No files in database. Run 'voom scan' first.").yellow()
            );
        } else if matches!(format, OutputFormat::Json) {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"total_files": 0, "total_size": 0})
                )
                .expect("json is serializable")
            );
        }
        return Ok(());
    };

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(snapshot).expect("snapshot is serializable")
            );
        }
        OutputFormat::Table => {
            use voom_domain::utils::format::{format_duration, format_size};

            let f = &snapshot.files;
            println!("{}", style("Library Summary").bold().underlined());
            println!(
                "  {} files, {}, {}",
                style(f.total_count).bold(),
                style(format_size(f.total_size_bytes)).cyan(),
                style(format_duration(f.total_duration_secs)).dim(),
            );

            if !f.container_counts.is_empty() {
                let top: Vec<_> = f.container_counts.iter().take(5).collect();
                let labels: Vec<String> = top.iter().map(|(n, c)| format!("{n} ({c})")).collect();
                println!("  Containers: {}", labels.join(", "));
            }
            if !snapshot.video.codec_counts.is_empty() {
                let top: Vec<_> = snapshot.video.codec_counts.iter().take(5).collect();
                let labels: Vec<String> = top.iter().map(|(n, c)| format!("{n} ({c})")).collect();
                println!("  Video codecs: {}", labels.join(", "));
            }
        }
        OutputFormat::Plain => {
            let f = &snapshot.files;
            println!("total_files={}", f.total_count);
            println!("total_size={}", f.total_size_bytes);
            println!("total_duration_secs={:.1}", f.total_duration_secs);
        }
        OutputFormat::Csv => {
            write_summary_csv(snapshot)?;
        }
    }
    Ok(())
}

fn write_summary_csv(snapshot: &LibrarySnapshot) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# library")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["total_files", "total_size_bytes", "total_duration_secs"])?;
    wtr.write_record([
        snapshot.files.total_count.to_string(),
        snapshot.files.total_size_bytes.to_string(),
        format!("{:.1}", snapshot.files.total_duration_secs),
    ])?;
    wtr.flush()?;
    Ok(())
}

fn format_result(result: &ReportResult, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).expect("report result is serializable")
            );
        }
        OutputFormat::Table => format_result_table(result),
        OutputFormat::Plain => format_result_plain(result),
        OutputFormat::Csv => format_result_csv(result)?,
    }
    Ok(())
}

// ── Table formatting ────────────────────────────────────────

const PLANS_TITLE: &str = "Plan Processing Summary";
const PLANS_EMPTY_HINT: &str = "No plans recorded yet. Run 'voom process' to generate plans.";

const SAVINGS_TITLE: &str = "Space Savings by Provenance";
const SAVINGS_EMPTY_HINT: &str = "No completed plans with size deltas yet.";

const HISTORY_TITLE: &str = "Snapshot History";
const HISTORY_EMPTY_HINT: &str = "No snapshots captured yet.";

const ISSUES_TITLE: &str = "Safeguard Violations";
const ISSUES_EMPTY_HINT: &str = "No safeguard violations found.";

fn print_empty_section(title: &str, hint: &str) {
    eprintln!("{}", style(title).bold().underlined());
    eprintln!("  {}", style(hint).dim());
    eprintln!();
}

fn format_result_table(result: &ReportResult) {
    if let Some(ref snapshot) = result.library {
        print_stats_table(snapshot);
    }
    if let Some(ref stats) = result.plans {
        print_plans_section_table(stats);
    }
    if let Some(ref report) = result.savings {
        print_savings_section_table(report);
    }
    if let Some(ref snapshots) = result.history {
        print_history_section_table(snapshots);
    }
    if let Some(ref issues) = result.issues {
        print_issues_section_table(issues);
    }
    if let Some(ref db) = result.database {
        print_database_section_table(db);
    }
}

fn print_stats_table(snapshot: &LibrarySnapshot) {
    use voom_domain::utils::format::{format_duration, format_size};

    let files = &snapshot.files;
    println!("{}", style("Library Overview").bold().underlined());
    println!(
        "  {} files, {}, {}",
        style(files.total_count).bold(),
        style(format_size(files.total_size_bytes)).cyan(),
        style(format_duration(files.total_duration_secs)).dim(),
    );
    println!(
        "  Avg size: {}  Max: {}  Min: {}",
        style(format_size(files.avg_size_bytes)).dim(),
        style(format_size(files.max_size_bytes)).dim(),
        style(format_size(files.min_size_bytes)).dim(),
    );
    println!();

    print_pair_table("Containers", "Container", "Count", &files.container_counts);

    let video = &snapshot.video;
    print_pair_table("Video Codecs", "Codec", "Count", &video.codec_counts);
    print_pair_table(
        "Video Resolutions",
        "Resolution",
        "Count",
        &video.resolution_counts,
    );
    println!(
        "  HDR: {}  VFR: {}",
        style(video.hdr_count).bold(),
        style(video.vfr_count).bold(),
    );
    println!();

    let audio = &snapshot.audio;
    print_pair_table("Audio Types", "Type", "Count", &audio.type_counts);
    let top_langs: Vec<_> = audio.language_counts.iter().take(20).cloned().collect();
    print_pair_table("Audio Languages (top 20)", "Language", "Count", &top_langs);
    print_pair_table("Audio Codecs", "Codec", "Count", &audio.codec_counts);

    let subs = &snapshot.subtitles;
    print_pair_table(
        "Subtitles by Language",
        "Language",
        "Count",
        &subs.language_counts,
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
        format!(
            "{} saved",
            voom_domain::utils::format::format_size(
                u64::try_from(p.total_size_saved_bytes).unwrap_or(0)
            )
        )
    } else {
        format!(
            "{} added",
            voom_domain::utils::format::format_size(p.total_size_saved_bytes.unsigned_abs())
        )
    };
    #[allow(clippy::cast_precision_loss)] // millisecond totals for reporting are well under 2^52
    let seconds = p.total_processing_time_ms as f64 / 1000.0;
    println!(
        "  Total time: {}  Size change: {}",
        style(voom_domain::utils::format::format_duration(seconds)).dim(),
        style(size_label).dim(),
    );
    println!();

    let jobs = &snapshot.jobs;
    if !jobs.by_status.is_empty() {
        print_pair_table("Jobs", "Status", "Count", &jobs.by_status);
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

#[derive(Default)]
struct PhaseAgg {
    completed: u64,
    skipped: u64,
    failed: u64,
    skip_reasons: HashMap<String, u64>,
}

fn aggregate_plan_stats(stats: &[PlanPhaseStat]) -> (Vec<String>, HashMap<String, PhaseAgg>) {
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

fn print_plans_section_table(stats: &[PlanPhaseStat]) {
    if stats.is_empty() {
        print_empty_section(PLANS_TITLE, PLANS_EMPTY_HINT);
        return;
    }
    let (phases, by_phase) = aggregate_plan_stats(stats);

    println!("{}", style(PLANS_TITLE).bold().underlined());
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
    println!();
}

fn print_savings_section_table(report: &SavingsReport) {
    use voom_domain::utils::format::{format_duration, format_size};

    if report.buckets.is_empty() {
        print_empty_section(SAVINGS_TITLE, SAVINGS_EMPTY_HINT);
        return;
    }

    let show_period = report.buckets.iter().any(|b| b.period.is_some());

    println!("{}", style(SAVINGS_TITLE).bold().underlined());
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
            format_size(u64::try_from(b.bytes_saved).unwrap_or(0))
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
        #[allow(clippy::cast_precision_loss)] // bucket durations stay well under 2^52 ms
        let seconds = b.duration_ms as f64 / 1000.0;
        row.extend_from_slice(&[
            Cell::new(b.file_count),
            Cell::new(b.transition_count),
            Cell::new(&saved_label),
            Cell::new(format_duration(seconds)),
        ]);
        table.add_row(row);
    }
    println!("{table}");

    let total_label = if report.total_bytes_saved >= 0 {
        format!(
            "{} saved",
            format_size(u64::try_from(report.total_bytes_saved).unwrap_or(0))
        )
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
    println!();
}

fn print_history_section_table(snapshots: &[LibrarySnapshot]) {
    use voom_domain::utils::format::{format_duration, format_size};

    if snapshots.is_empty() {
        print_empty_section(HISTORY_TITLE, HISTORY_EMPTY_HINT);
        return;
    }

    println!("{}", style(HISTORY_TITLE).bold().underlined());
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
    for snap in snapshots {
        table.add_row(vec![
            Cell::new(snap.captured_at.format("%Y-%m-%d %H:%M:%S")),
            Cell::new(snap.trigger.as_str()),
            Cell::new(snap.files.total_count),
            Cell::new(format_size(snap.files.total_size_bytes)),
            Cell::new(format_duration(snap.files.total_duration_secs)),
            Cell::new(snap.video.hdr_count),
            Cell::new(snap.video.vfr_count),
        ]);
    }
    println!("{table}");
    println!();
}

fn print_issues_section_table(issues: &[IssueReport]) {
    if issues.is_empty() {
        print_empty_section(ISSUES_TITLE, ISSUES_EMPTY_HINT);
        return;
    }

    println!(
        "{} ({} files)",
        style(ISSUES_TITLE).bold().underlined(),
        issues.len()
    );
    println!();
    let mut table = output::new_table();
    table.set_header(vec!["Path", "Violation", "Phase", "Message"]);
    for issue in issues {
        let path = issue
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        for v in &issue.violations {
            table.add_row(vec![
                Cell::new(&path),
                Cell::new(v.kind.as_str()),
                Cell::new(&v.phase_name),
                Cell::new(&v.message),
            ]);
        }
    }
    println!("{table}");
    println!();
}

fn print_database_section_table(db: &DatabaseStats) {
    use voom_domain::utils::format::format_size;

    println!("{}", style("Database").bold().underlined());
    println!();

    if !db.table_counts.is_empty() {
        let mut table = output::new_table();
        table.set_header(vec!["Table", "Rows"]);
        for (name, count) in &db.table_counts {
            table.add_row(vec![Cell::new(name), Cell::new(count)]);
        }
        println!("{table}");
    }

    let ps = &db.page_stats;
    let total = ps.page_size * ps.page_count;
    let free = ps.page_size * ps.freelist_count;
    println!(
        "  Page size: {}  Pages: {}  Total: {}  Free: {}",
        style(format_size(ps.page_size)).dim(),
        style(ps.page_count).dim(),
        style(format_size(total)).dim(),
        style(format_size(free)).dim(),
    );
    println!();
}

fn print_retention_footer(store: &dyn voom_domain::storage::StorageTrait) {
    if let Ok(Some(record)) = store.latest_event_of_type("retention.completed") {
        if let Ok(voom_domain::events::Event::RetentionCompleted(e)) =
            serde_json::from_str::<voom_domain::events::Event>(&record.payload)
        {
            println!();
            println!(
                "Last retention pass: {:?} ({} ms)",
                e.trigger, e.duration_ms
            );
            for table in &e.per_table {
                if let Some(ref err) = table.error {
                    println!("  {:20} ERROR: {err}", table.table);
                } else {
                    println!(
                        "  {:20} deleted={} kept={}",
                        table.table, table.deleted, table.kept
                    );
                }
            }
        }
    }
}

// ── Plain formatting ────────────────────────────────────────

fn format_result_plain(result: &ReportResult) {
    if let Some(ref snapshot) = result.library {
        print_stats_plain(snapshot);
    }
    if let Some(ref stats) = result.plans {
        print_plans_section_plain(stats);
    }
    if let Some(ref report) = result.savings {
        print_savings_section_plain(report);
    }
    if let Some(ref snapshots) = result.history {
        print_history_section_plain(snapshots);
    }
    if let Some(ref issues) = result.issues {
        print_issues_section_plain(issues);
    }
    if let Some(ref db) = result.database {
        print_database_section_plain(db);
    }
}

fn print_stats_plain(snapshot: &LibrarySnapshot) {
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

fn print_plans_section_plain(plan_stats: &[PlanPhaseStat]) {
    if plan_stats.is_empty() {
        return;
    }
    let (phases, by_phase) = aggregate_plan_stats(plan_stats);
    for name in &phases {
        let ps = by_phase.get(name).expect("phase exists");
        let label = if ps.failed > 0 {
            "failed"
        } else if ps.completed > 0 {
            "completed"
        } else {
            "skipped"
        };
        println!("{name}\t{label}");
    }
}

fn print_savings_section_plain(report: &SavingsReport) {
    let show_period = report.buckets.iter().any(|b| b.period.is_some());
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

fn print_history_section_plain(snapshots: &[LibrarySnapshot]) {
    for snap in snapshots {
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

fn print_issues_section_plain(issues: &[IssueReport]) {
    for issue in issues {
        for v in &issue.violations {
            println!("{}\t{}", issue.path.display(), v.kind.as_str());
        }
    }
}

fn print_database_section_plain(db: &DatabaseStats) {
    for (name, count) in &db.table_counts {
        println!("table_{name}={count}");
    }
    println!("page_size={}", db.page_stats.page_size);
    println!("page_count={}", db.page_stats.page_count);
    println!("freelist_count={}", db.page_stats.freelist_count);
}

// ── CSV formatting ──────────────────────────────────────────

fn format_result_csv(result: &ReportResult) -> Result<()> {
    if let Some(ref snapshot) = result.library {
        write_library_csv(snapshot)?;
    }
    if let Some(ref stats) = result.plans {
        write_plans_csv(stats)?;
    }
    if let Some(ref report) = result.savings {
        write_savings_csv(report)?;
    }
    if let Some(ref snapshots) = result.history {
        write_history_csv(snapshots)?;
    }
    if let Some(ref issues) = result.issues {
        write_issues_csv(issues)?;
    }
    if let Some(ref db) = result.database {
        write_database_csv(db)?;
    }
    Ok(())
}

fn write_library_csv(snapshot: &LibrarySnapshot) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# library")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "total_files",
        "total_size_bytes",
        "total_duration_secs",
        "avg_size_bytes",
        "max_size_bytes",
        "min_size_bytes",
    ])?;
    wtr.write_record([
        snapshot.files.total_count.to_string(),
        snapshot.files.total_size_bytes.to_string(),
        format!("{:.1}", snapshot.files.total_duration_secs),
        snapshot.files.avg_size_bytes.to_string(),
        snapshot.files.max_size_bytes.to_string(),
        snapshot.files.min_size_bytes.to_string(),
    ])?;
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(out, "# containers")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["container", "count"])?;
    for (name, count) in &snapshot.files.container_counts {
        wtr.write_record([name.as_str(), &count.to_string()])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(out, "# video_codecs")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["codec", "count"])?;
    for (name, count) in &snapshot.video.codec_counts {
        wtr.write_record([name.as_str(), &count.to_string()])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

fn write_plans_csv(stats: &[PlanPhaseStat]) -> Result<()> {
    let (phases, by_phase) = aggregate_plan_stats(stats);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# plans")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["phase", "completed", "skipped", "failed"])?;
    for name in &phases {
        let ps = by_phase.get(name).expect("phase exists");
        wtr.write_record([
            name.as_str(),
            &ps.completed.to_string(),
            &ps.skipped.to_string(),
            &ps.failed.to_string(),
        ])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

fn write_savings_csv(report: &SavingsReport) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# savings")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "executor",
        "phase",
        "period",
        "file_count",
        "transition_count",
        "bytes_saved",
        "duration_ms",
    ])?;
    for b in &report.buckets {
        wtr.write_record([
            b.executor.as_deref().unwrap_or(""),
            b.phase.as_deref().unwrap_or(""),
            b.period.as_deref().unwrap_or(""),
            &b.file_count.to_string(),
            &b.transition_count.to_string(),
            &b.bytes_saved.to_string(),
            &b.duration_ms.to_string(),
        ])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

fn write_history_csv(snapshots: &[LibrarySnapshot]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# history")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "timestamp",
        "trigger",
        "files",
        "total_size_bytes",
        "total_duration_secs",
        "hdr_count",
        "vfr_count",
    ])?;
    for snap in snapshots {
        wtr.write_record([
            snap.captured_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            snap.trigger.as_str().to_string(),
            snap.files.total_count.to_string(),
            snap.files.total_size_bytes.to_string(),
            format!("{:.0}", snap.files.total_duration_secs),
            snap.video.hdr_count.to_string(),
            snap.video.vfr_count.to_string(),
        ])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

fn write_issues_csv(issues: &[IssueReport]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# issues")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["path", "violation", "phase", "message"])?;
    for issue in issues {
        for v in &issue.violations {
            wtr.write_record([
                &issue.path.display().to_string(),
                v.kind.as_str(),
                &v.phase_name,
                &v.message,
            ])?;
        }
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

fn write_database_csv(db: &DatabaseStats) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# database_tables")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["table", "rows"])?;
    for (name, count) in &db.table_counts {
        wtr.write_record([name.as_str(), &count.to_string()])?;
    }
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(out, "# database_pages")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record(["page_size", "page_count", "freelist_count"])?;
    wtr.write_record([
        db.page_stats.page_size.to_string(),
        db.page_stats.page_count.to_string(),
        db.page_stats.freelist_count.to_string(),
    ])?;
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);

    Ok(())
}

// ── File list ───────────────────────────────────────────────

fn run_file_list(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let files = store
        .list_files(&voom_domain::FileFilters::default())
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

// ── Integrity summary ──────────────────────────────────────

const INTEGRITY_STALE_DAYS: i64 = 30;

fn run_integrity(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let since_cutoff = chrono::Utc::now() - chrono::Duration::days(INTEGRITY_STALE_DAYS);
    let summary = store
        .integrity_summary(since_cutoff)
        .context("failed to query integrity summary")?;

    match format {
        OutputFormat::Json => print_integrity_json(&summary, since_cutoff)?,
        OutputFormat::Table => print_integrity_table(&summary, since_cutoff),
        OutputFormat::Plain => print_integrity_plain(&summary),
        OutputFormat::Csv => print_integrity_csv(&summary)?,
    }
    Ok(())
}

fn run_integrity_and_vmaf(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let since_cutoff = chrono::Utc::now() - chrono::Duration::days(INTEGRITY_STALE_DAYS);
    let integrity = store
        .integrity_summary(since_cutoff)
        .context("failed to query integrity summary")?;
    let vmaf = build_vmaf_summary(store).context("failed to query VMAF summary")?;

    match format {
        OutputFormat::Json => print_integrity_and_vmaf_json(&integrity, since_cutoff, &vmaf)?,
        OutputFormat::Table => {
            print_integrity_table(&integrity, since_cutoff);
            print_vmaf_table(&vmaf);
        }
        OutputFormat::Plain => {
            print_integrity_plain(&integrity);
            print_vmaf_plain(&vmaf);
        }
        OutputFormat::Csv => {
            print_integrity_csv(&integrity)?;
            print_vmaf_csv(&vmaf)?;
        }
    }
    Ok(())
}

fn print_integrity_table(summary: &IntegritySummary, since_cutoff: chrono::DateTime<chrono::Utc>) {
    println!(
        "{} (stale cutoff: {})",
        style("Library Integrity").bold().underlined(),
        since_cutoff.format("%Y-%m-%d"),
    );
    println!();
    let mut table = output::new_table();
    table.set_header(vec!["Metric", "Count"]);
    table.add_row(vec![
        Cell::new("Total files"),
        Cell::new(summary.total_files),
    ]);
    table.add_row(vec![
        Cell::new("Never verified"),
        Cell::new(summary.never_verified),
    ]);
    table.add_row(vec![
        Cell::new(format!("Stale (> {INTEGRITY_STALE_DAYS}d)")),
        Cell::new(summary.stale),
    ]);
    table.add_row(vec![
        Cell::new("With errors"),
        Cell::new(summary.with_errors),
    ]);
    table.add_row(vec![
        Cell::new("With warnings"),
        Cell::new(summary.with_warnings),
    ]);
    table.add_row(vec![
        Cell::new("Hash mismatches"),
        Cell::new(summary.hash_mismatches),
    ]);
    println!("{table}");
    println!();
}

fn print_integrity_plain(summary: &IntegritySummary) {
    println!("total_files={}", summary.total_files);
    println!("never_verified={}", summary.never_verified);
    println!("stale={}", summary.stale);
    println!("with_errors={}", summary.with_errors);
    println!("with_warnings={}", summary.with_warnings);
    println!("hash_mismatches={}", summary.hash_mismatches);
}

fn print_integrity_json(
    summary: &IntegritySummary,
    since_cutoff: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let payload = integrity_json_value(summary, since_cutoff);
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).context("serialize integrity summary")?
    );
    Ok(())
}

fn print_integrity_and_vmaf_json(
    integrity: &IntegritySummary,
    since_cutoff: chrono::DateTime<chrono::Utc>,
    vmaf: &VmafSummary,
) -> Result<()> {
    let payload = serde_json::json!({
        "integrity": integrity_json_value(integrity, since_cutoff),
        "vmaf": vmaf,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&payload).context("serialize report summaries")?
    );
    Ok(())
}

fn integrity_json_value(
    summary: &IntegritySummary,
    since_cutoff: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    serde_json::json!({
        "stale_cutoff": since_cutoff.to_rfc3339(),
        "stale_cutoff_days": INTEGRITY_STALE_DAYS,
        "summary": summary,
    })
}

fn print_integrity_csv(summary: &IntegritySummary) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# integrity")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "total_files",
        "never_verified",
        "stale",
        "with_errors",
        "with_warnings",
        "hash_mismatches",
    ])?;
    wtr.write_record([
        summary.total_files.to_string(),
        summary.never_verified.to_string(),
        summary.stale.to_string(),
        summary.with_errors.to_string(),
        summary.with_warnings.to_string(),
        summary.hash_mismatches.to_string(),
    ])?;
    wtr.flush()?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    drop(out);
    Ok(())
}

// ── VMAF summary ───────────────────────────────────────────

const VMAF_BOTTOM_LIMIT: usize = 20;
const VMAF_MISS_THRESHOLD: f64 = 1.0;

#[derive(Debug, Serialize)]
struct VmafSummary {
    total_transcodes: usize,
    average_target_vmaf: Option<f64>,
    average_achieved_vmaf: Option<f64>,
    average_iterations: Option<f64>,
    missed_target_count: usize,
    fallback_count: usize,
    fallback_rate: Option<f64>,
    bottom_files: Vec<VmafBottomFile>,
}

#[derive(Debug, Serialize)]
struct VmafBottomFile {
    file_id: String,
    path: Option<String>,
    target_vmaf: u32,
    achieved_vmaf: f64,
    deficit: f64,
    iterations: u32,
    fallback_used: bool,
}

fn run_vmaf(store: &dyn voom_domain::storage::StorageTrait, format: OutputFormat) -> Result<()> {
    let summary = build_vmaf_summary(store).context("failed to query VMAF summary")?;
    match format {
        OutputFormat::Json => print_vmaf_json(&summary)?,
        OutputFormat::Table => print_vmaf_table(&summary),
        OutputFormat::Plain => print_vmaf_plain(&summary),
        OutputFormat::Csv => print_vmaf_csv(&summary)?,
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct LoudnessSummary {
    measured_tracks: usize,
    average_integrated_lufs: Option<f64>,
    average_true_peak_db: Option<f64>,
    outside_broadcast_target: usize,
    outliers: Vec<LoudnessOutlier>,
}

#[derive(Debug, Serialize)]
struct LoudnessOutlier {
    path: String,
    track_index: u32,
    integrated_lufs: f64,
    true_peak_db: Option<f64>,
}

fn run_loudness(
    store: &dyn voom_domain::storage::StorageTrait,
    format: OutputFormat,
) -> Result<()> {
    let files = store
        .list_files(&voom_domain::storage::FileFilters::default())
        .context("failed to query files for loudness report")?;
    let mut values = Vec::new();
    let mut peaks = Vec::new();
    let mut outliers = Vec::new();
    for file in files {
        for track in file
            .tracks
            .iter()
            .filter(|track| track.track_type.is_audio())
        {
            let Some(lufs) = track.loudness_integrated_lufs else {
                continue;
            };
            values.push(lufs);
            if let Some(peak) = track.loudness_true_peak_db {
                peaks.push(peak);
            }
            if (lufs + 23.0).abs() > 0.5 {
                outliers.push(LoudnessOutlier {
                    path: file.path.display().to_string(),
                    track_index: track.index,
                    integrated_lufs: round2(lufs),
                    true_peak_db: track.loudness_true_peak_db.map(round2),
                });
            }
        }
    }
    let outside_broadcast_target = outliers.len();
    outliers.sort_by(|a, b| {
        let a_drift = (a.integrated_lufs + 23.0).abs();
        let b_drift = (b.integrated_lufs + 23.0).abs();
        b_drift.total_cmp(&a_drift)
    });
    outliers.truncate(10);
    let summary = LoudnessSummary {
        measured_tracks: values.len(),
        average_integrated_lufs: average_option(values.iter().copied()).map(round2),
        average_true_peak_db: average_option(peaks.into_iter()).map(round2),
        outside_broadcast_target,
        outliers,
    };
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        OutputFormat::Table => print_loudness_table(&summary),
        OutputFormat::Plain => {
            println!("measured_tracks={}", summary.measured_tracks);
            println!(
                "average_integrated_lufs={}",
                format_optional_score(summary.average_integrated_lufs)
            );
            println!(
                "average_true_peak_db={}",
                format_optional_score(summary.average_true_peak_db)
            );
            println!(
                "outside_broadcast_target={}",
                summary.outside_broadcast_target
            );
        }
        OutputFormat::Csv => {
            let mut out = std::io::stdout();
            writeln!(
                out,
                "measured_tracks,average_integrated_lufs,average_true_peak_db,outside_broadcast_target"
            )?;
            writeln!(
                out,
                "{},{},{},{}",
                summary.measured_tracks,
                format_optional_score(summary.average_integrated_lufs),
                format_optional_score(summary.average_true_peak_db),
                summary.outside_broadcast_target
            )?;
        }
    }
    Ok(())
}

fn print_loudness_table(summary: &LoudnessSummary) {
    println!("{}", style("Loudness Summary").bold().underlined());
    let mut table = comfy_table::Table::new();
    table.set_header(vec!["Metric", "Value"]);
    table.add_row(vec![
        Cell::new("Measured audio tracks"),
        Cell::new(summary.measured_tracks),
    ]);
    table.add_row(vec![
        Cell::new("Average integrated LUFS"),
        Cell::new(format_optional_score(summary.average_integrated_lufs)),
    ]);
    table.add_row(vec![
        Cell::new("Average true peak dB"),
        Cell::new(format_optional_score(summary.average_true_peak_db)),
    ]);
    table.add_row(vec![
        Cell::new("Outside -23 LUFS +/-0.5"),
        Cell::new(summary.outside_broadcast_target),
    ]);
    println!("{table}");
}

fn build_vmaf_summary(store: &dyn voom_domain::storage::StorageTrait) -> Result<VmafSummary> {
    let filters = TranscodeOutcomeFilters::default();
    let outcomes = store.list_transcode_outcomes(&filters)?;
    let targeted: Vec<_> = outcomes
        .into_iter()
        .filter(|outcome| outcome.target_vmaf.is_some())
        .collect();
    if targeted.is_empty() {
        return Ok(VmafSummary::empty());
    }

    let total_transcodes = targeted.len();
    let fallback_count = targeted
        .iter()
        .filter(|outcome| outcome.fallback_used)
        .count();
    let bottom_files = build_vmaf_bottom_files(store, &targeted)?;
    let missed_target_count = bottom_files
        .iter()
        .filter(|file| file.deficit > VMAF_MISS_THRESHOLD)
        .count();
    let achieved: Vec<f64> = targeted
        .iter()
        .filter_map(|outcome| outcome.achieved_vmaf.map(f64::from))
        .collect();

    Ok(VmafSummary {
        total_transcodes,
        average_target_vmaf: Some(round2(average(
            targeted
                .iter()
                .filter_map(|outcome| outcome.target_vmaf.map(f64::from)),
        ))),
        average_achieved_vmaf: average_option(achieved.into_iter()),
        average_iterations: Some(round2(average(
            targeted.iter().map(|outcome| f64::from(outcome.iterations)),
        ))),
        missed_target_count,
        fallback_count,
        fallback_rate: Some(round4(fallback_count as f64 / total_transcodes as f64)),
        bottom_files,
    })
}

impl VmafSummary {
    fn empty() -> Self {
        Self {
            total_transcodes: 0,
            average_target_vmaf: None,
            average_achieved_vmaf: None,
            average_iterations: None,
            missed_target_count: 0,
            fallback_count: 0,
            fallback_rate: None,
            bottom_files: Vec::new(),
        }
    }
}

fn build_vmaf_bottom_files(
    store: &dyn voom_domain::storage::StorageTrait,
    outcomes: &[TranscodeOutcome],
) -> Result<Vec<VmafBottomFile>> {
    let mut files = Vec::new();
    for outcome in outcomes {
        let (Some(target), Some(achieved)) = (outcome.target_vmaf, outcome.achieved_vmaf) else {
            continue;
        };
        let deficit = f64::from(target) - f64::from(achieved);
        if deficit <= 0.0 {
            continue;
        }
        files.push(VmafBottomFile {
            file_id: outcome.file_id.clone(),
            path: vmaf_file_path(store, &outcome.file_id)?,
            target_vmaf: target,
            achieved_vmaf: round2(f64::from(achieved)),
            deficit: round2(deficit),
            iterations: outcome.iterations,
            fallback_used: outcome.fallback_used,
        });
    }
    files.sort_by(compare_vmaf_bottom_files);
    files.truncate(VMAF_BOTTOM_LIMIT);
    Ok(files)
}

fn vmaf_file_path(
    store: &dyn voom_domain::storage::StorageTrait,
    file_id: &str,
) -> Result<Option<String>> {
    let Ok(uuid) = uuid::Uuid::parse_str(file_id) else {
        return Ok(None);
    };
    Ok(store
        .file(&uuid)?
        .map(|file| file.path.display().to_string()))
}

fn compare_vmaf_bottom_files(a: &VmafBottomFile, b: &VmafBottomFile) -> std::cmp::Ordering {
    b.deficit
        .total_cmp(&a.deficit)
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.file_id.cmp(&b.file_id))
}

fn average(values: impl Iterator<Item = f64>) -> f64 {
    let mut total = 0.0;
    let mut count = 0usize;
    for value in values {
        total += value;
        count += 1;
    }
    total / count as f64
}

fn average_option(values: impl Iterator<Item = f64>) -> Option<f64> {
    let values: Vec<_> = values.collect();
    if values.is_empty() {
        None
    } else {
        Some(round2(average(values.into_iter())))
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn format_optional_rate(value: Option<f64>) -> String {
    value
        .map(|rate| format!("{rate:.2}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn format_optional_score(value: Option<f64>) -> String {
    value
        .map(|score| format!("{score:.2}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn print_vmaf_table(summary: &VmafSummary) {
    if summary.total_transcodes == 0 {
        println!("no VMAF-guided transcodes recorded yet");
        return;
    }

    println!("{}", style("VMAF-Guided Transcodes").bold().underlined());
    println!();
    let mut table = output::new_table();
    table.set_header(vec!["Metric", "Value"]);
    table.add_row(vec![
        "Total transcodes",
        &summary.total_transcodes.to_string(),
    ]);
    table.add_row(vec![
        "Average target VMAF",
        &format_optional_score(summary.average_target_vmaf),
    ]);
    table.add_row(vec![
        "Average achieved VMAF",
        &format_optional_score(summary.average_achieved_vmaf),
    ]);
    table.add_row(vec![
        "Average iterations",
        &format_optional_score(summary.average_iterations),
    ]);
    table.add_row(vec![
        "Missed target",
        &summary.missed_target_count.to_string(),
    ]);
    table.add_row(vec![
        "Fallback rate",
        &format_optional_rate(summary.fallback_rate),
    ]);
    println!("{table}");
    print_vmaf_bottom_table(summary);
}

fn print_vmaf_bottom_table(summary: &VmafSummary) {
    if summary.bottom_files.is_empty() {
        return;
    }
    println!();
    let mut table = output::new_table();
    table.set_header(vec![
        "Path",
        "Target",
        "Achieved",
        "Deficit",
        "Iterations",
        "Fallback",
    ]);
    for file in &summary.bottom_files {
        table.add_row(vec![
            file.path.as_deref().unwrap_or(&file.file_id),
            &file.target_vmaf.to_string(),
            &format!("{:.2}", file.achieved_vmaf),
            &format!("{:.2}", file.deficit),
            &file.iterations.to_string(),
            if file.fallback_used { "yes" } else { "no" },
        ]);
    }
    println!("{table}");
}

fn print_vmaf_plain(summary: &VmafSummary) {
    if summary.total_transcodes == 0 {
        println!("no VMAF-guided transcodes recorded yet");
        return;
    }
    println!("total_transcodes={}", summary.total_transcodes);
    println!(
        "average_target_vmaf={}",
        format_optional_score(summary.average_target_vmaf)
    );
    println!(
        "average_achieved_vmaf={}",
        format_optional_score(summary.average_achieved_vmaf)
    );
    println!(
        "average_iterations={}",
        format_optional_score(summary.average_iterations)
    );
    println!("missed_target_count={}", summary.missed_target_count);
    println!("fallback_count={}", summary.fallback_count);
    println!(
        "fallback_rate={}",
        format_optional_rate(summary.fallback_rate)
    );
    for file in &summary.bottom_files {
        println!(
            "bottom_file\t{}\t{:.2}\t{}\t{:.2}\t{}",
            file.path.as_deref().unwrap_or(&file.file_id),
            file.deficit,
            file.target_vmaf,
            file.achieved_vmaf,
            file.iterations
        );
    }
}

fn print_vmaf_json(summary: &VmafSummary) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(summary).context("serialize VMAF summary")?
    );
    Ok(())
}

fn print_vmaf_csv(summary: &VmafSummary) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "# vmaf_summary")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "total_transcodes",
        "average_target_vmaf",
        "average_achieved_vmaf",
        "average_iterations",
        "missed_target_count",
        "fallback_count",
        "fallback_rate",
    ])?;
    wtr.write_record([
        summary.total_transcodes.to_string(),
        format_optional_score(summary.average_target_vmaf),
        format_optional_score(summary.average_achieved_vmaf),
        format_optional_score(summary.average_iterations),
        summary.missed_target_count.to_string(),
        summary.fallback_count.to_string(),
        format_optional_rate(summary.fallback_rate),
    ])?;
    wtr.flush()?;
    print_vmaf_bottom_csv(summary)
}

fn print_vmaf_bottom_csv(summary: &VmafSummary) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out)?;
    writeln!(out, "# vmaf_bottom_files")?;
    drop(out);

    let stdout = std::io::stdout();
    let mut wtr = csv::Writer::from_writer(stdout.lock());
    wtr.write_record([
        "path",
        "file_id",
        "target_vmaf",
        "achieved_vmaf",
        "deficit",
        "iterations",
        "fallback_used",
    ])?;
    for file in &summary.bottom_files {
        wtr.write_record([
            file.path.clone().unwrap_or_default(),
            file.file_id.clone(),
            file.target_vmaf.to_string(),
            format!("{:.2}", file.achieved_vmaf),
            format!("{:.2}", file.deficit),
            file.iterations.to_string(),
            file.fallback_used.to_string(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

// ── Error reporting ────────────────────────────────────────

// This handler threads through multiple error-report modes (list sessions,
// filter by session / plan / format) and relies on shared local state;
// splitting it further would require threading every filter separately.
#[allow(clippy::too_many_lines)]
fn run_errors(
    store: &dyn voom_domain::storage::StorageTrait,
    args: &ReportArgs,
    format: OutputFormat,
) -> Result<()> {
    use voom_domain::plan::ExecutionDetail;

    if args.list_sessions {
        let sessions = store.failure_sessions()?;
        if sessions.is_empty() {
            eprintln!("{}", style("No sessions with errors found.").dim());
            return Ok(());
        }
        if let OutputFormat::Json = format {
            println!(
                "{}",
                serde_json::to_string_pretty(&sessions).context("serialize sessions")?
            );
        } else {
            println!("{}", style("Sessions with errors:").bold().underlined());
            println!();
            for s in &sessions {
                let short = &s.session_id.to_string()[..8];
                println!(
                    "  {} {} ({} failures)",
                    style(short).cyan(),
                    style(s.started_at.format("%Y-%m-%d %H:%M:%S")).dim(),
                    style(s.failure_count).yellow(),
                );
            }
        }
        return Ok(());
    }

    let session_id = if let Some(ref s) = args.session {
        uuid::Uuid::parse_str(s).or_else(|_| {
            // Allow short prefix matching
            let sessions = store.failure_sessions()?;
            sessions
                .iter()
                .find(|sess| sess.session_id.to_string().starts_with(s))
                .map(|sess| sess.session_id)
                .ok_or_else(|| anyhow::anyhow!("no session matching prefix '{s}'"))
        })?
    } else {
        store
            .latest_failure_session()
            .context("failed to query latest session")?
            .ok_or_else(|| anyhow::anyhow!("no sessions with errors found"))?
    };

    let failures = store
        .failed_transitions_for_session(&session_id)
        .context("failed to query session errors")?;

    if failures.is_empty() {
        eprintln!("{}", style("No errors in the most recent session.").green());
        return Ok(());
    }

    if let OutputFormat::Json = format {
        println!(
            "{}",
            serde_json::to_string_pretty(&failures).context("serialize failures")?
        );
    } else {
        let short_session = &session_id.to_string()[..8];
        println!(
            "Errors from session {} ({} failures)\n",
            style(short_session).cyan(),
            failures.len(),
        );
        for f in &failures {
            let filename = f.path.file_name().map_or_else(
                || f.path.display().to_string(),
                |n| n.to_string_lossy().to_string(),
            );
            println!("  {}", style(&filename).bold());
            if let Some(ref phase) = f.phase_name {
                println!("    Phase: {phase}");
            }

            // Try to extract ExecutionDetail from plan_result JSON
            let mut detail_rendered = false;
            if let Some(ref result_json) = f.plan_result {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(result_json) {
                    if let Some(detail) = parsed.get("detail") {
                        if let Ok(ed) = serde_json::from_value::<ExecutionDetail>(detail.clone()) {
                            if let Some(code) = ed.exit_code {
                                println!("    Exit:  {code}");
                                detail_rendered = true;
                            }
                            if !ed.command.is_empty() {
                                let cmd = console::strip_ansi_codes(&ed.command);
                                println!("    Cmd:   {cmd}");
                                detail_rendered = true;
                            }
                            if !ed.stderr_tail.is_empty() {
                                let stderr = console::strip_ansi_codes(&ed.stderr_tail);
                                println!("    Error:");
                                for line in stderr.lines() {
                                    println!("      {line}");
                                }
                                detail_rendered = true;
                            }
                        }
                    }
                }
            }

            if let Some(ref msg) = f.error_message {
                if !detail_rendered {
                    println!("    Error: {msg}");
                }
            }
            println!();
        }
    }

    Ok(())
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

    #[test]
    fn test_build_request_no_flags_gives_summary() {
        let args = ReportArgs {
            format: OutputFormat::Table,
            library: false,
            plans: false,
            savings: false,
            period: None,
            history: None,
            issues: false,
            database: false,
            all: false,
            snapshot: false,
            files: false,
            integrity: false,
            vmaf: false,
            loudness: false,
            errors: false,
            session: None,
            list_sessions: false,
        };
        let req = build_request(&args).unwrap();
        assert!(req.includes(ReportSection::Library));
        assert!(!req.includes(ReportSection::Plans));
    }

    #[test]
    fn test_build_request_all_flag() {
        let args = ReportArgs {
            format: OutputFormat::Table,
            library: false,
            plans: false,
            savings: false,
            period: None,
            history: None,
            issues: false,
            database: false,
            all: true,
            snapshot: false,
            files: false,
            integrity: false,
            vmaf: false,
            loudness: false,
            errors: false,
            session: None,
            list_sessions: false,
        };
        let req = build_request(&args).unwrap();
        assert!(req.includes(ReportSection::Library));
        assert!(req.includes(ReportSection::Plans));
        assert!(req.includes(ReportSection::Database));
    }

    #[test]
    fn test_build_request_specific_sections() {
        let args = ReportArgs {
            format: OutputFormat::Table,
            library: false,
            plans: true,
            savings: false,
            period: None,
            history: Some(10),
            issues: false,
            database: false,
            all: false,
            snapshot: false,
            files: false,
            integrity: false,
            vmaf: false,
            loudness: false,
            errors: false,
            session: None,
            list_sessions: false,
        };
        let req = build_request(&args).unwrap();
        assert!(req.includes(ReportSection::Plans));
        assert!(req.includes(ReportSection::History));
        assert!(!req.includes(ReportSection::Library));
        assert_eq!(req.history_limit, Some(10));
    }

    #[test]
    fn test_run_integrity_on_empty_store() {
        use voom_domain::test_support::InMemoryStore;
        let store = InMemoryStore::new();
        // All formats must succeed against an empty store; integrity_summary
        // returns the default (all zeros) so no rows are missing.
        for format in [
            OutputFormat::Table,
            OutputFormat::Plain,
            OutputFormat::Json,
            OutputFormat::Csv,
        ] {
            run_integrity(&store, format).expect("integrity summary on empty store");
        }
    }

    #[test]
    fn test_is_summary_request() {
        let default_args = ReportArgs {
            format: OutputFormat::Table,
            library: false,
            plans: false,
            savings: false,
            period: None,
            history: None,
            issues: false,
            database: false,
            all: false,
            snapshot: false,
            files: false,
            integrity: false,
            vmaf: false,
            loudness: false,
            errors: false,
            session: None,
            list_sessions: false,
        };
        assert!(is_summary_request(&default_args));

        let specific_args = ReportArgs {
            plans: true,
            ..default_args
        };
        assert!(!is_summary_request(&specific_args));
    }
}
