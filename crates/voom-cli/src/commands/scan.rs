use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use voom_domain::storage::StorageTrait;

use crate::app;
use crate::cli::ScanArgs;
use crate::output;

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
    let options = voom_discovery::ScanOptions {
        root: path,
        recursive: args.recursive,
        hash_files: !args.no_hash,
        workers: args.workers,
    };

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message("Discovering media files...");
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

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
        ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("#>-"),
    );

    let mut introspected = 0u64;
    let mut errors = 0u64;

    for event in &events {
        pb.set_message(
            event
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );

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

    println!(
        "\n{} {} files discovered, {} introspected{}",
        "Done.".bold().green(),
        events.len(),
        introspected,
        if errors > 0 {
            format!(", {} {}", errors, "errors".red())
        } else {
            String::new()
        }
    );

    // Show summary table
    let results: Vec<_> = events
        .iter()
        .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
        .collect();
    output::format_scan_results(&results, crate::cli::OutputFormat::Table);

    Ok(())
}
