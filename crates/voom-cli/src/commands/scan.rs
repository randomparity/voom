use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::app;
use crate::cli::ScanArgs;
use crate::config;
use crate::output;
use crate::paths::resolve_paths;
use crate::progress::{DiscoveryProgress, ProbeProgress};
use anyhow::{Context, Result};
use console::style;
use indicatif::HumanDuration;
use tokio_util::sync::CancellationToken;
use voom_domain::bad_file::BadFileSource;
use voom_domain::events::Event;

/// Run the scan command.
///
/// Discovery and introspection are driven directly for deterministic progress
/// reporting, but all events are also published through the kernel's event bus
/// so that subscribers (sqlite-store, WASM plugins) receive them.
pub async fn run(args: ScanArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let app::BootstrapResult { kernel, store, .. } = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);

    let paths = resolve_paths(&args.paths)?;

    if !quiet {
        let path_list: Vec<_> = paths
            .iter()
            .map(|p| style(p.display()).cyan().to_string())
            .collect();
        eprintln!("{} {}", style("Scanning").bold(), path_list.join(", "));
    }

    let discovery = voom_discovery::DiscoveryPlugin::new();

    let progress = if quiet {
        DiscoveryProgress::hidden()
    } else {
        DiscoveryProgress::new()
    };
    let orphan_count = Arc::new(AtomicU64::new(0));
    let discovery_errors = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let hash_files = !args.no_hash;

    let mut all_events = Vec::new();

    // Cumulative counters so the progress bar shows totals across all
    // directories instead of resetting per directory.
    let cumulative_discovered = Arc::new(AtomicU64::new(0));
    let processing_base = Arc::new(AtomicU64::new(0));

    for path in &paths {
        // Reset bar to spinner so stale position/length from the previous
        // directory's processing phase doesn't bleed into this discovery.
        progress.reset_to_spinner();

        let progress_clone = progress.clone();
        let orphan_count_clone = orphan_count.clone();
        let discovery_errors_clone = discovery_errors.clone();
        let kernel_for_errors = kernel.clone();
        let cum_disc = cumulative_discovered.clone();
        let proc_base = processing_base.clone();

        let pre_scan_discovered = cumulative_discovered.load(Ordering::Relaxed);

        let mut options = voom_discovery::ScanOptions::new(path.clone());
        options.recursive = args.recursive;
        options.hash_files = hash_files;
        options.workers = args.workers;
        options.on_progress = Some(Box::new(move |progress| match progress {
            voom_discovery::ScanProgress::Discovered { count: _, path } => {
                let cumulative = cum_disc.fetch_add(1, Ordering::Relaxed) + 1;
                progress_clone.on_discovered(cumulative as usize, &path);
            }
            voom_discovery::ScanProgress::Processing {
                current,
                total,
                path,
            } => {
                let base = proc_base.load(Ordering::Relaxed) as usize;
                let action = if hash_files { "Hashing" } else { "Processing" };
                progress_clone.on_processing(base + current, base + total, &path, action);
            }
            voom_discovery::ScanProgress::OrphanedTempFiles { count } => {
                orphan_count_clone.fetch_add(count as u64, Ordering::Relaxed);
            }
        }));
        options.on_error = Some(Box::new(move |path, size, error| {
            tracing::warn!(path = %path.display(), error = %error, "discovery error");
            discovery_errors_clone.fetch_add(1, Ordering::Relaxed);
            crate::introspect::dispatch_failure(
                &kernel_for_errors,
                path,
                size,
                None,
                &error,
                BadFileSource::Discovery,
            );
        }));

        let events = discovery.scan(&options).context("filesystem scan failed")?;

        // Update processing base so the next directory's progress continues
        // from where this one left off.
        let dir_discovered = cumulative_discovered.load(Ordering::Relaxed) - pre_scan_discovered;
        processing_base.fetch_add(dir_discovered, Ordering::Relaxed);

        all_events.extend(events);
    }

    progress.finish();

    // Deduplicate events by path in case multiple scan roots overlap
    let mut seen = std::collections::HashSet::new();
    all_events.retain(|e| seen.insert(e.path.clone()));

    let orphans = orphan_count.load(Ordering::Relaxed);
    let disc_errors = discovery_errors.load(Ordering::Relaxed);

    if all_events.is_empty() {
        // Even with no files on disk, run reconciliation so that previously
        // known files under the scanned directories are marked missing.
        if hash_files {
            let reconcile_result = store.reconcile_discovered_files(&[], &paths)?;
            if !quiet && reconcile_result.missing > 0 {
                eprintln!(
                    "  {} {} files no longer on disk",
                    style("Missing").dim(),
                    reconcile_result.missing
                );
            }
        } else {
            let missing = store.mark_missing_paths(&[], &paths)?;
            if !quiet && missing > 0 {
                eprintln!(
                    "  {} {} files no longer on disk",
                    style("Missing").dim(),
                    missing
                );
            }
        }

        if !quiet {
            if orphans > 0 {
                eprintln!(
                    "{} ({} orphaned temp {} skipped)",
                    style("No media files found.").yellow(),
                    orphans,
                    if orphans == 1 { "file" } else { "files" },
                );
            } else {
                eprintln!("{}", style("No media files found.").yellow());
            }
        }
        if matches!(args.format, Some(crate::cli::OutputFormat::Json)) {
            println!("[]");
        }
        return Ok(());
    }

    // Show discovery/hashing summary
    if !quiet {
        let discovery_elapsed = start.elapsed();
        let orphan_suffix = if orphans > 0 {
            format!(
                " ({} orphaned temp {} skipped)",
                orphans,
                if orphans == 1 { "file" } else { "files" }
            )
        } else {
            String::new()
        };
        let disc_error_suffix = if disc_errors > 0 {
            format!(
                ", {} discovery {}",
                disc_errors,
                if disc_errors == 1 { "error" } else { "errors" }
            )
        } else {
            String::new()
        };
        if hash_files {
            let elapsed_ms = discovery_elapsed.as_millis();
            let elapsed_str = if elapsed_ms < 1000 {
                format!("{elapsed_ms}ms")
            } else {
                format!("{}", HumanDuration(discovery_elapsed))
            };
            eprintln!(
                "  {} {} files, hashed in {}{}{}",
                style("Discovered").dim(),
                all_events.len(),
                elapsed_str,
                orphan_suffix,
                disc_error_suffix,
            );
        } else {
            eprintln!(
                "  {} {} files (hashing skipped){}{}",
                style("Discovered").dim(),
                all_events.len(),
                orphan_suffix,
                disc_error_suffix,
            );
        }
    }

    // Mark missing files — path-only, no hash needed.
    // When hashing is enabled, reconcile_discovered_files handles this internally.
    // When hashing is disabled, we still need to detect deleted files.
    let path_missing_count = if !hash_files {
        let discovered_paths: Vec<std::path::PathBuf> =
            all_events.iter().map(|e| e.path.clone()).collect();
        store.mark_missing_paths(&discovered_paths, &paths)?
    } else {
        0
    };

    if !quiet && path_missing_count > 0 {
        eprintln!(
            "  {} {} files no longer on disk",
            style("Missing").dim(),
            path_missing_count
        );
    }

    // Batch reconciliation: detect moves, external changes, and missing files.
    // Requires content hashes — skip if --no-hash was specified.
    let reconcile_introspect_paths: Option<std::collections::HashSet<std::path::PathBuf>> =
        if hash_files {
            let discovered: Vec<voom_domain::transition::DiscoveredFile> = all_events
                .iter()
                .filter_map(|e| {
                    e.content_hash.as_ref().map(|hash| {
                        voom_domain::transition::DiscoveredFile::new(
                            e.path.clone(),
                            e.size,
                            hash.clone(),
                        )
                    })
                })
                .collect();

            let reconcile_result = store.reconcile_discovered_files(&discovered, &paths)?;

            if !quiet {
                if reconcile_result.missing > 0 {
                    eprintln!(
                        "  {} {} files no longer on disk",
                        style("Missing").dim(),
                        reconcile_result.missing
                    );
                }
                if reconcile_result.moved > 0 {
                    eprintln!(
                        "  {} {} files moved/renamed",
                        style("Moved").dim(),
                        reconcile_result.moved
                    );
                }
                if reconcile_result.external_changes > 0 {
                    eprintln!(
                        "  {} {} files changed externally",
                        style("Changed").dim(),
                        reconcile_result.external_changes
                    );
                }
            }

            Some(reconcile_result.needs_introspection.into_iter().collect())
        } else {
            None
        };

    // Dispatch FileDiscovered events through the kernel so subscribers react:
    // - sqlite-store records each file in the discovered_files staging table
    // - ffprobe-introspector enqueues JobType::Introspect jobs
    //
    // The CLI still drives introspection directly (below) for deterministic
    // progress reporting. The enqueued introspect jobs are not consumed here;
    // they exist for future daemon-mode use (issue #36).
    for event in &all_events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    // With hashing enabled, use reconciliation results to determine which files
    // need introspection (new, moved, externally changed). Without hashing,
    // introspect everything.
    let needs_introspection: Vec<_> = if let Some(ref introspect_set) = reconcile_introspect_paths {
        all_events
            .iter()
            .filter(|e| introspect_set.contains(&e.path))
            .collect()
    } else {
        all_events.iter().collect()
    };

    let probe = if quiet {
        ProbeProgress::hidden(needs_introspection.len())
    } else {
        ProbeProgress::new(needs_introspection.len())
    };
    let mut introspected = 0u64;
    let mut errors = 0u64;
    let total = all_events.len() as u64;

    for (i, event) in needs_introspection.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        probe.on_file(i + 1, &event.path);

        match crate::introspect::introspect_file(
            event.path.clone(),
            event.size,
            event.content_hash.clone(),
            &kernel,
            config.ffprobe_path(),
        )
        .await
        {
            Ok(_file) => {
                introspected += 1;
            }
            Err(e) => {
                tracing::warn!(path = %event.path.display(), error = %e, "introspection failed");
                errors += 1;
            }
        }

        probe.inc();
    }

    probe.finish();

    let total_elapsed = start.elapsed();
    if token.is_cancelled() {
        if !quiet {
            eprintln!(
                "\n{} {} files discovered, {}/{} introspected{} ({})",
                style("Interrupted.").bold().yellow(),
                all_events.len(),
                introspected,
                total,
                if errors > 0 {
                    format!(", {} {}", errors, style("errors").red())
                } else {
                    String::new()
                },
                HumanDuration(total_elapsed),
            );
        }
        // Emit valid output for machine formats even on interruption
        if let Some(format) = args.format {
            let results: Vec<_> = all_events
                .iter()
                .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
                .collect();
            output::format_scan_results(&results, format);
        }
        return Ok(());
    }

    if !quiet {
        eprintln!(
            "\n{} {} files discovered, {} introspected{} ({})",
            style("Done.").bold().green(),
            all_events.len(),
            introspected,
            if errors > 0 {
                format!(", {} {}", errors, style("errors").red())
            } else {
                String::new()
            },
            HumanDuration(total_elapsed),
        );
    }

    // Prune old missing records based on retention config.
    let retention_days = config.pruning.retention_days;
    if retention_days > 0 {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(retention_days));
        match store.purge_missing(cutoff) {
            Ok(n) if n > 0 && !quiet => {
                eprintln!(
                    "  {} {} stale records (missing >{} days)",
                    style("Purged").dim(),
                    n,
                    retention_days
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "purge failed"),
        }
    }

    // Capture a library snapshot for trend tracking
    capture_snapshot(&*store);

    if let Some(format) = args.format {
        let results: Vec<_> = all_events
            .iter()
            .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
            .collect();
        output::format_scan_results(&results, format);
    }

    Ok(())
}

fn capture_snapshot(store: &dyn voom_domain::storage::SnapshotStorage) {
    match store.gather_library_stats(voom_domain::stats::SnapshotTrigger::ScanComplete) {
        Ok(snapshot) => {
            if let Err(e) = store.save_snapshot(&snapshot) {
                tracing::warn!(error = %e, "failed to save library snapshot");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to gather library stats for snapshot");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{EventResult, FileDiscoveredEvent, FileIntrospectedEvent};
    use voom_domain::media::MediaFile;

    /// A test plugin that counts received events.
    struct RecordingPlugin {
        discovered_count: AtomicUsize,
        introspected_count: AtomicUsize,
    }

    impl RecordingPlugin {
        fn new() -> Self {
            Self {
                discovered_count: AtomicUsize::new(0),
                introspected_count: AtomicUsize::new(0),
            }
        }
    }

    impl voom_kernel::Plugin for RecordingPlugin {
        fn name(&self) -> &str {
            "test-recorder"
        }
        fn version(&self) -> &str {
            "0.1.0"
        }
        fn capabilities(&self) -> &[Capability] {
            &[]
        }
        fn handles(&self, event_type: &str) -> bool {
            matches!(
                event_type,
                Event::FILE_DISCOVERED | Event::FILE_INTROSPECTED
            )
        }
        fn on_event(&self, event: &Event) -> voom_domain::errors::Result<Option<EventResult>> {
            match event {
                Event::FileDiscovered(_) => {
                    self.discovered_count.fetch_add(1, Ordering::SeqCst);
                }
                Event::FileIntrospected(_) => {
                    self.introspected_count.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }
            Ok(None)
        }
    }

    fn test_media_file(name: &str) -> MediaFile {
        let mut f = MediaFile::new(PathBuf::from(name));
        f.size = 1024;
        f.content_hash = Some("abc123".into());
        f
    }

    #[tokio::test]
    async fn test_events_dispatched_through_kernel() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        // Simulate discovery event
        let discovered =
            FileDiscoveredEvent::new(PathBuf::from("/tmp/test.mkv"), 1024, Some("abc123".into()));
        kernel.dispatch(Event::FileDiscovered(discovered));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);

        // Simulate introspection event
        let file = test_media_file("/tmp/test.mkv");
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent::new(file)));

        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_discovery_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50).unwrap();

        let events = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 100, Some("aaa".into())),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/b.mp4"), 200, Some("bbb".into())),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/c.avi"), 300, Some("ccc".into())),
        ];

        for event in &events {
            kernel.dispatch(Event::FileDiscovered(event.clone()));
        }

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 3);
    }
}
