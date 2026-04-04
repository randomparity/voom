use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use voom_domain::events::{Event, FileDiscoveredEvent};

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
    let hash_files = !args.no_hash;
    let start = Instant::now();

    if !quiet {
        let path_list: Vec<_> = paths
            .iter()
            .map(|p| style(p.display()).cyan().to_string())
            .collect();
        eprintln!("{} {}", style("Scanning").bold(), path_list.join(", "));
    }

    let (mut all_events, orphans, disc_errors) =
        run_discovery(&args, &paths, hash_files, quiet, &kernel)?;

    // Deduplicate events by path in case multiple scan roots overlap
    let mut seen = HashSet::new();
    all_events.retain(|e| seen.insert(e.path.clone()));

    if all_events.is_empty() {
        handle_empty_scan(&*store, &paths, hash_files, orphans, quiet, args.format)?;
        return Ok(());
    }

    if !quiet {
        print_discovery_summary(
            all_events.len(),
            start.elapsed(),
            hash_files,
            orphans,
            disc_errors,
        );
    }

    mark_missing_without_hash(&*store, &all_events, &paths, hash_files, quiet)?;
    let reconcile_paths = reconcile_files(&*store, &all_events, &paths, hash_files, quiet)?;

    // Dispatch FileDiscovered events through the kernel so subscribers react.
    for event in &all_events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    let needs_introspection = filter_for_introspection(&all_events, &reconcile_paths);
    let (introspected, errors) = run_introspection(
        &needs_introspection,
        &kernel,
        config.ffprobe_path(),
        &token,
        quiet,
    )
    .await;

    print_scan_summary(
        &all_events,
        introspected,
        errors,
        start.elapsed(),
        token.is_cancelled(),
        quiet,
        args.format,
    );

    if token.is_cancelled() {
        return Ok(());
    }

    purge_stale_records(&*store, config.pruning.retention_days, quiet);
    capture_snapshot(&*store);

    if let Some(format) = args.format {
        output::format_scan_results(&format_results(&all_events), format);
    }

    Ok(())
}

/// Run filesystem discovery across all paths, returning events and counters.
fn run_discovery(
    args: &ScanArgs,
    paths: &[PathBuf],
    hash_files: bool,
    quiet: bool,
    kernel: &Arc<voom_kernel::Kernel>,
) -> Result<(Vec<FileDiscoveredEvent>, u64, u64)> {
    let discovery = voom_discovery::DiscoveryPlugin::new();
    let progress = if quiet {
        DiscoveryProgress::hidden()
    } else {
        DiscoveryProgress::new()
    };
    let orphan_count = Arc::new(AtomicU64::new(0));
    let discovery_errors = Arc::new(AtomicU64::new(0));
    let cumulative_discovered = Arc::new(AtomicU64::new(0));
    let processing_base = Arc::new(AtomicU64::new(0));
    let mut all_events = Vec::new();

    for path in paths {
        progress.reset_to_spinner();

        let progress_clone = progress.clone();
        let orphan_clone = orphan_count.clone();
        let errors_clone = discovery_errors.clone();
        let kernel_clone = kernel.clone();
        let cum_disc = cumulative_discovered.clone();
        let proc_base = processing_base.clone();
        let pre_scan = cumulative_discovered.load(Ordering::Relaxed);

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
                orphan_clone.fetch_add(count as u64, Ordering::Relaxed);
            }
        }));
        options.on_error = Some(Box::new(move |path, size, error| {
            tracing::warn!(path = %path.display(), error = %error, "discovery error");
            errors_clone.fetch_add(1, Ordering::Relaxed);
            crate::introspect::dispatch_failure(
                &kernel_clone,
                path,
                size,
                None,
                &error,
                BadFileSource::Discovery,
            );
        }));

        let events = discovery.scan(&options).context("filesystem scan failed")?;

        let dir_discovered = cumulative_discovered.load(Ordering::Relaxed) - pre_scan;
        processing_base.fetch_add(dir_discovered, Ordering::Relaxed);
        all_events.extend(events);
    }

    progress.finish();

    let orphans = orphan_count.load(Ordering::Relaxed);
    let errors = discovery_errors.load(Ordering::Relaxed);
    Ok((all_events, orphans, errors))
}

/// Handle the case where discovery found no files. Runs reconciliation
/// to mark previously known files as missing, then prints output.
fn handle_empty_scan(
    store: &dyn voom_domain::storage::StorageTrait,
    paths: &[PathBuf],
    hash_files: bool,
    orphans: u64,
    quiet: bool,
    format: Option<crate::cli::OutputFormat>,
) -> Result<()> {
    if hash_files {
        let result = store.reconcile_discovered_files(&[], paths)?;
        if !quiet && result.missing > 0 {
            print_missing_count(result.missing);
        }
    } else {
        let missing = store.mark_missing_paths(&[], paths)?;
        if !quiet && missing > 0 {
            print_missing_count(missing);
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

    if matches!(format, Some(crate::cli::OutputFormat::Json)) {
        println!("[]");
    }
    Ok(())
}

/// Print the discovery/hashing summary line.
fn print_discovery_summary(
    file_count: usize,
    elapsed: Duration,
    hash_files: bool,
    orphans: u64,
    disc_errors: u64,
) {
    let orphan_suffix = if orphans > 0 {
        format!(
            " ({} orphaned temp {} skipped)",
            orphans,
            if orphans == 1 { "file" } else { "files" }
        )
    } else {
        String::new()
    };
    let error_suffix = if disc_errors > 0 {
        format!(
            ", {} discovery {}",
            disc_errors,
            if disc_errors == 1 { "error" } else { "errors" }
        )
    } else {
        String::new()
    };

    if hash_files {
        let elapsed_str = if elapsed.as_millis() < 1000 {
            format!("{}ms", elapsed.as_millis())
        } else {
            format!("{}", HumanDuration(elapsed))
        };
        eprintln!(
            "  {} {} files, hashed in {}{}{}",
            style("Discovered").dim(),
            file_count,
            elapsed_str,
            orphan_suffix,
            error_suffix,
        );
    } else {
        eprintln!(
            "  {} {} files (hashing skipped){}{}",
            style("Discovered").dim(),
            file_count,
            orphan_suffix,
            error_suffix,
        );
    }
}

/// Mark files as missing using path-only comparison (no hash needed).
/// Only runs when hashing is disabled; with hashing, reconcile handles it.
fn mark_missing_without_hash(
    store: &dyn voom_domain::storage::StorageTrait,
    events: &[FileDiscoveredEvent],
    paths: &[PathBuf],
    hash_files: bool,
    quiet: bool,
) -> Result<()> {
    if hash_files {
        return Ok(());
    }
    let discovered: Vec<PathBuf> = events.iter().map(|e| e.path.clone()).collect();
    let missing = store.mark_missing_paths(&discovered, paths)?;
    if !quiet && missing > 0 {
        print_missing_count(missing);
    }
    Ok(())
}

/// Batch reconciliation: detect moves, external changes, and missing files.
/// Returns the set of paths needing introspection, or None if hashing is off.
fn reconcile_files(
    store: &dyn voom_domain::storage::StorageTrait,
    events: &[FileDiscoveredEvent],
    paths: &[PathBuf],
    hash_files: bool,
    quiet: bool,
) -> Result<Option<HashSet<PathBuf>>> {
    if !hash_files {
        return Ok(None);
    }

    let discovered: Vec<voom_domain::transition::DiscoveredFile> = events
        .iter()
        .filter_map(|e| {
            e.content_hash.as_ref().map(|hash| {
                voom_domain::transition::DiscoveredFile::new(e.path.clone(), e.size, hash.clone())
            })
        })
        .collect();

    let result = store.reconcile_discovered_files(&discovered, paths)?;

    if !quiet {
        if result.missing > 0 {
            print_missing_count(result.missing);
        }
        if result.moved > 0 {
            eprintln!(
                "  {} {} files moved/renamed",
                style("Moved").dim(),
                result.moved
            );
        }
        if result.external_changes > 0 {
            eprintln!(
                "  {} {} files changed externally",
                style("Changed").dim(),
                result.external_changes
            );
        }
    }

    Ok(Some(result.needs_introspection.into_iter().collect()))
}

/// Filter events to only those needing introspection based on reconciliation.
fn filter_for_introspection<'a>(
    events: &'a [FileDiscoveredEvent],
    reconcile_paths: &'a Option<HashSet<PathBuf>>,
) -> Vec<&'a FileDiscoveredEvent> {
    if let Some(set) = reconcile_paths {
        events.iter().filter(|e| set.contains(&e.path)).collect()
    } else {
        events.iter().collect()
    }
}

/// Run ffprobe introspection on files. Returns (introspected, errors) counts.
async fn run_introspection(
    events: &[&FileDiscoveredEvent],
    kernel: &Arc<voom_kernel::Kernel>,
    ffprobe_path: Option<&str>,
    token: &CancellationToken,
    quiet: bool,
) -> (u64, u64) {
    let probe = if quiet {
        ProbeProgress::hidden(events.len())
    } else {
        ProbeProgress::new(events.len())
    };
    let mut introspected = 0u64;
    let mut errors = 0u64;

    for (i, event) in events.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        probe.on_file(i + 1, &event.path);

        match crate::introspect::introspect_file(
            event.path.clone(),
            event.size,
            event.content_hash.clone(),
            kernel,
            ffprobe_path,
        )
        .await
        {
            Ok(_file) => introspected += 1,
            Err(e) => {
                tracing::warn!(
                    path = %event.path.display(),
                    error = %e,
                    "introspection failed"
                );
                errors += 1;
            }
        }
        probe.inc();
    }

    probe.finish();
    (introspected, errors)
}

/// Print the final scan summary (completion or interruption).
fn print_scan_summary(
    events: &[FileDiscoveredEvent],
    introspected: u64,
    errors: u64,
    elapsed: Duration,
    cancelled: bool,
    quiet: bool,
    format: Option<crate::cli::OutputFormat>,
) {
    let total = events.len() as u64;
    let error_suffix = if errors > 0 {
        format!(", {} {}", errors, style("errors").red())
    } else {
        String::new()
    };

    if cancelled {
        if !quiet {
            eprintln!(
                "\n{} {} files discovered, {}/{} introspected{} ({})",
                style("Interrupted.").bold().yellow(),
                events.len(),
                introspected,
                total,
                error_suffix,
                HumanDuration(elapsed),
            );
        }
        if let Some(format) = format {
            output::format_scan_results(&format_results(events), format);
        }
        return;
    }

    if !quiet {
        eprintln!(
            "\n{} {} files discovered, {} introspected{} ({})",
            style("Done.").bold().green(),
            events.len(),
            introspected,
            error_suffix,
            HumanDuration(elapsed),
        );
    }
}

/// Purge stale missing records based on retention config.
fn purge_stale_records(
    store: &dyn voom_domain::storage::StorageTrait,
    retention_days: u32,
    quiet: bool,
) {
    if retention_days == 0 {
        return;
    }
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

fn print_missing_count(count: u32) {
    eprintln!(
        "  {} {} files no longer on disk",
        style("Missing").dim(),
        count
    );
}

fn format_results(events: &[FileDiscoveredEvent]) -> Vec<(PathBuf, u64, Option<String>)> {
    events
        .iter()
        .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
        .collect()
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
