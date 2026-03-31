use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::app;
use crate::cli::ScanArgs;
use crate::config;
use crate::output;
use crate::progress::{DiscoveryProgress, ProbeProgress};
use anyhow::{Context, Result};
use console::style;
use indicatif::HumanDuration;
use tokio_util::sync::CancellationToken;

/// Run the scan command.
///
/// Discovery and introspection are driven directly for deterministic progress
/// reporting, but all events are also published through the kernel's event bus
/// so that subscribers (sqlite-store, WASM plugins) receive them.
pub async fn run(args: ScanArgs, quiet: bool, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let app::BootstrapResult { kernel, store, .. } = app::bootstrap_kernel_with_store(&config)?;
    let kernel = Arc::new(kernel);

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    // Auto-prune stale file entries under the scanned directory
    match store.prune_missing_files_under(&path) {
        Ok(n) if n > 0 && !quiet => eprintln!("Pruned {n} stale entries."),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "auto-prune failed"),
    }

    if !quiet {
        eprintln!(
            "{} {}",
            style("Scanning").bold(),
            style(path.display()).cyan()
        );
    }

    let discovery = voom_discovery::DiscoveryPlugin::new();

    let progress = if quiet {
        DiscoveryProgress::hidden()
    } else {
        DiscoveryProgress::new()
    };
    let progress_clone = progress.clone();
    let file_count = Arc::new(AtomicU64::new(0));
    let file_count_clone = file_count.clone();
    let start = Instant::now();
    let hash_files = !args.no_hash;

    let mut options = voom_discovery::ScanOptions::new(path);
    options.recursive = args.recursive;
    options.hash_files = hash_files;
    options.workers = args.workers;
    options.on_progress = Some(Box::new(move |progress| match progress {
        voom_discovery::ScanProgress::Discovered { count, path } => {
            progress_clone.on_discovered(count, &path);
        }
        voom_discovery::ScanProgress::Processing {
            current,
            total,
            path,
        } => {
            let action = if hash_files { "Hashing" } else { "Processing" };
            progress_clone.on_processing(current, total, &path, action);
            file_count_clone.store(total as u64, Ordering::Relaxed);
        }
    }));

    let events = discovery.scan(&options).context("filesystem scan failed")?;

    progress.finish();

    if events.is_empty() {
        if !quiet {
            eprintln!("{}", style("No media files found.").yellow());
        }
        if matches!(args.format, Some(crate::cli::OutputFormat::Json)) {
            println!("[]");
        }
        return Ok(());
    }

    // Show discovery/hashing summary
    if !quiet {
        let discovery_elapsed = start.elapsed();
        if hash_files {
            let elapsed_ms = discovery_elapsed.as_millis();
            let elapsed_str = if elapsed_ms < 1000 {
                format!("{elapsed_ms}ms")
            } else {
                format!("{}", HumanDuration(discovery_elapsed))
            };
            eprintln!(
                "  {} {} files, hashed in {}",
                style("Discovered").dim(),
                events.len(),
                elapsed_str,
            );
        } else {
            eprintln!(
                "  {} {} files (hashing skipped)",
                style("Discovered").dim(),
                events.len(),
            );
        }
    }

    // Dispatch FileDiscovered events through the kernel so subscribers react:
    // - sqlite-store records each file in the discovered_files staging table
    // - ffprobe-introspector enqueues JobType::Introspect jobs
    //
    // The CLI still drives introspection directly (below) for deterministic
    // progress reporting. The enqueued introspect jobs are not consumed here;
    // they exist for future daemon-mode use (issue #36).
    for event in &events {
        kernel.dispatch(voom_domain::events::Event::FileDiscovered(event.clone()));
    }

    let probe = if quiet {
        ProbeProgress::hidden(events.len())
    } else {
        ProbeProgress::new(events.len())
    };
    let mut introspected = 0u64;
    let mut errors = 0u64;
    let total = events.len() as u64;

    for (i, event) in events.iter().enumerate() {
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
                events.len(),
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
            let results: Vec<_> = events
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
            events.len(),
            introspected,
            if errors > 0 {
                format!(", {} {}", errors, style("errors").red())
            } else {
                String::new()
            },
            HumanDuration(total_elapsed),
        );
    }

    if let Some(format) = args.format {
        let results: Vec<_> = events
            .iter()
            .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
            .collect();
        output::format_scan_results(&results, format);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use voom_domain::capabilities::Capability;
    use voom_domain::events::{Event, EventResult, FileDiscoveredEvent, FileIntrospectedEvent};
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
