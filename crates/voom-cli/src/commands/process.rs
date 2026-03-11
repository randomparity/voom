use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use crate::app;
use crate::cli::ProcessArgs;
use crate::output;

pub async fn run(args: ProcessArgs) -> Result<()> {
    let _config = app::load_config()?;

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    // Load and compile the policy
    let policy_source = std::fs::read_to_string(&args.policy)
        .with_context(|| format!("Failed to read policy: {}", args.policy.display()))?;

    let compiled = voom_dsl::compile(&policy_source).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "{} policy {} to {}{}",
        if args.dry_run {
            "Dry-running".bold()
        } else {
            "Applying".bold()
        },
        compiled.name.cyan(),
        path.display().to_string().cyan(),
        if args.dry_run {
            " (no changes will be made)"
        } else {
            ""
        }
    );

    // Discover files
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let options = voom_discovery::ScanOptions {
        root: path.clone(),
        recursive: true,
        hash_files: !args.no_backup,
        workers: args.workers,
    };

    let events = discovery
        .scan(&options)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
    }

    println!("Found {} media files.", events.len().to_string().bold());

    // Process each file
    let introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
    let orchestrator = voom_phase_orchestrator::PhaseOrchestratorPlugin::new();

    let pb = ProgressBar::new(events.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("#>-"),
    );

    let mut processed = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    for event in &events {
        let filename = event
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        pb.set_message(filename);

        // Introspect
        let file = match introspector.introspect(&event.path, event.size, &event.content_hash) {
            Ok(intro) => intro.file,
            Err(e) => {
                tracing::warn!(path = %event.path.display(), error = %e, "introspection failed");
                errors += 1;
                pb.inc(1);
                continue;
            }
        };

        // Orchestrate
        match orchestrator.orchestrate(&compiled, &file) {
            Ok(result) => {
                if args.dry_run {
                    pb.suspend(|| {
                        println!("\n{}", event.path.display().to_string().bold().underline());
                        output::format_plans(&result.plans);
                    });
                }

                if voom_phase_orchestrator::PhaseOrchestratorPlugin::needs_execution(&result) {
                    processed += 1;
                } else {
                    skipped += 1;
                }
            }
            Err(e) => {
                tracing::warn!(path = %event.path.display(), error = %e, "orchestration failed");
                errors += 1;
            }
        }

        pb.inc(1);
    }

    pb.finish_and_clear();

    println!(
        "\n{} {} processed, {} skipped, {} errors",
        "Done.".bold().green(),
        processed.to_string().green(),
        skipped.to_string().dimmed(),
        if errors > 0 {
            errors.to_string().red().to_string()
        } else {
            errors.to_string()
        }
    );

    if args.dry_run {
        println!(
            "\n{}",
            "This was a dry run. No files were modified.".dimmed()
        );
    }

    Ok(())
}
