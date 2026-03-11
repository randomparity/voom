use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use voom_domain::storage::StorageTrait;

use crate::app;
use crate::cli::ScanArgs;
use crate::output::{self, max_filename_len, shrink_filename};

/// Fixed-width overhead of the progress bar line (spinner + bar + counters + percent + padding).
const PROGRESS_FIXED_WIDTH: usize = 77;

/// Run the scan command.
///
/// This function calls the discovery and introspector plugins directly rather than
/// routing through the event bus. This direct-call pattern is intentional for CLI
/// commands: it enables deterministic progress reporting (indicatif progress bars),
/// sequential error handling, and avoids the async overhead of the pub/sub bus for
/// what is fundamentally a synchronous, user-facing workflow.
pub async fn run(args: ScanArgs) -> Result<()> {
    let config = app::load_config()?;

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    println!(
        "{} {}",
        "Scanning".bold(),
        path.display().to_string().cyan()
    );

    let discovery = voom_discovery::DiscoveryPlugin::new();

    // Set up a progress bar that transitions from discovery → processing
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let pb_clone = pb.clone();
    let file_count = Arc::new(AtomicU64::new(0));
    let file_count_clone = file_count.clone();
    let start = Instant::now();

    let options = voom_discovery::ScanOptions {
        root: path,
        recursive: args.recursive,
        hash_files: !args.no_hash,
        workers: args.workers,
        on_progress: Some(Box::new(move |progress| {
            match progress {
                voom_discovery::ScanProgress::Discovered { count, path } => {
                    let max_name = max_filename_len(30); // "⠋ Discovering... N files found — "
                    let name = path
                        .file_name()
                        .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
                        .unwrap_or_default();
                    pb_clone
                        .set_message(format!("Discovering... {} files found — {}", count, name));
                }
                voom_discovery::ScanProgress::Processing {
                    current,
                    total,
                    path,
                } => {
                    // Switch to determinate progress on first processing event
                    if current == 1 {
                        pb_clone.set_length(total as u64);
                        pb_clone.set_style(
                            ProgressStyle::with_template(
                                "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}"
                            )
                            .unwrap()
                            .progress_chars("#>-"),
                        );
                    }
                    let elapsed = start.elapsed().as_secs_f64();
                    let rate = current as f64 / elapsed;
                    let remaining = (total - current) as f64 / rate;
                    let eta = if remaining.is_finite() && remaining > 0.0 {
                        format!(
                            ", ETA {}",
                            HumanDuration(std::time::Duration::from_secs(remaining as u64))
                        )
                    } else {
                        String::new()
                    };
                    let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
                    let name = path
                        .file_name()
                        .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
                        .unwrap_or_default();
                    pb_clone.set_position(current as u64);
                    pb_clone.set_message(format!("{}{}", name, eta));
                    file_count_clone.store(total as u64, Ordering::Relaxed);
                }
            }
        })),
    };

    let events = discovery
        .scan(&options)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    pb.finish_and_clear();

    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
    }

    // Introspect each discovered file
    let introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    let store = app::open_store(&config)?;

    let pb = ProgressBar::new(events.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}",
        )
        .unwrap()
        .progress_chars("#>-"),
    );
    pb.set_message("Introspecting...");

    let intro_start = Instant::now();
    let mut introspected = 0u64;
    let mut errors = 0u64;
    let total = events.len() as u64;

    for (i, event) in events.iter().enumerate() {
        let elapsed = intro_start.elapsed().as_secs_f64();
        let done = (i + 1) as f64;
        let rate = done / elapsed;
        let remaining = (total as f64 - done) / rate;
        let eta = if remaining.is_finite() && remaining > 0.0 && i > 0 {
            format!(
                ", ETA {}",
                HumanDuration(std::time::Duration::from_secs(remaining as u64))
            )
        } else {
            String::new()
        };

        let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
        let name = event
            .path
            .file_name()
            .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
            .unwrap_or_default();

        pb.set_message(format!("{}{}", name, eta));

        match introspector.introspect(&event.path, event.size, &event.content_hash) {
            Ok(intro_event) => {
                if let Err(e) = store.upsert_file(&intro_event.file) {
                    tracing::warn!(path = %event.path.display(), error = %e, "failed to store file");
                    errors += 1;
                } else {
                    introspected += 1;
                }
            }
            Err(e) => {
                tracing::warn!(path = %event.path.display(), error = %e, "introspection failed");
                errors += 1;
            }
        }

        pb.inc(1);
    }

    pb.finish_and_clear();

    let total_elapsed = start.elapsed();
    println!(
        "\n{} {} files discovered, {} introspected{} ({})",
        "Done.".bold().green(),
        events.len(),
        introspected,
        if errors > 0 {
            format!(", {} {}", errors, "errors".red())
        } else {
            String::new()
        },
        HumanDuration(total_elapsed),
    );

    // Show summary table
    let results: Vec<_> = events
        .iter()
        .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
        .collect();
    output::format_scan_results(&results, crate::cli::OutputFormat::Table);

    Ok(())
}
