use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::app;
use crate::cli::ScanArgs;
use crate::config;
use crate::output::{self, max_filename_len, shrink_filename, PROGRESS_FIXED_WIDTH};
use anyhow::{Context, Result};
use console::style;
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use tokio_util::sync::CancellationToken;

/// Format an ETA string from elapsed time and progress counts.
/// Returns an empty string when ETA cannot be meaningfully computed.
fn format_eta(elapsed: Duration, current: usize, total: usize) -> String {
    if current == 0 {
        return String::new();
    }
    let elapsed = elapsed.as_secs_f64();
    let rate = current as f64 / elapsed;
    let remaining = (total - current) as f64 / rate;
    if remaining.is_finite() && remaining > 0.0 {
        format!(
            ", ETA {}",
            HumanDuration(std::time::Duration::from_secs(remaining as u64))
        )
    } else {
        String::new()
    }
}

/// Run the scan command.
///
/// Discovery and introspection are driven directly for deterministic progress
/// reporting, but all events are also published through the kernel's event bus
/// so that subscribers (sqlite-store, WASM plugins) receive them.
pub async fn run(args: ScanArgs, token: CancellationToken) -> Result<()> {
    let config = config::load_config()?;
    let app::BootstrapResult { kernel, store, .. } = app::bootstrap_kernel_with_store(&config)?;

    let path = args
        .path
        .canonicalize()
        .with_context(|| format!("Path not found: {}", args.path.display()))?;

    // Auto-prune stale file entries under the scanned directory
    match store.prune_missing_files_under(&path) {
        Ok(n) if n > 0 => println!("Pruned {n} stale entries."),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "auto-prune failed"),
    }

    println!(
        "{} {}",
        style("Scanning").bold(),
        style(path.display()).cyan()
    );

    let discovery = voom_discovery::DiscoveryPlugin::new();

    // Set up a progress bar that transitions from discovery → processing
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .expect("valid progress template")
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.enable_steady_tick(std::time::Duration::from_millis(80));

    let pb_clone = pb.clone();
    let file_count = Arc::new(AtomicU64::new(0));
    let file_count_clone = file_count.clone();
    let start = Instant::now();
    let hash_files = !args.no_hash;

    let mut options = voom_discovery::ScanOptions::new(path);
    options.recursive = args.recursive;
    options.hash_files = hash_files;
    options.workers = args.workers;
    options.on_progress = Some(Box::new(move |progress| {
        match progress {
            voom_discovery::ScanProgress::Discovered { count, path } => {
                // 2 = spinner + space; the rest is the message prefix
                let prefix = format!("Discovering... {count} files found — ");
                let max_name = max_filename_len(2 + prefix.len());
                let name = path
                    .file_name()
                    .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
                    .unwrap_or_default();
                pb_clone.set_message(format!("{prefix}{name}"));
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
                            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}",
                        )
                        .expect("valid progress template")
                        .progress_chars("#>-"),
                    );
                }
                let eta = format_eta(start.elapsed(), current, total);
                let prefix = if hash_files { "Hashing" } else { "Processing" };
                let max_name =
                    max_filename_len(PROGRESS_FIXED_WIDTH + eta.len() + prefix.len() + 3);
                let name = path
                    .file_name()
                    .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
                    .unwrap_or_default();
                pb_clone.set_position(current as u64);
                pb_clone.set_message(format!("{prefix} {name}{eta}"));
                file_count_clone.store(total as u64, Ordering::Relaxed);
            }
        }
    }));

    let events = discovery.scan(&options).context("filesystem scan failed")?;

    pb.finish_and_clear();

    if events.is_empty() {
        println!("{}", style("No media files found.").yellow());
        return Ok(());
    }

    // Show discovery/hashing summary
    let discovery_elapsed = start.elapsed();
    if hash_files {
        let elapsed_ms = discovery_elapsed.as_millis();
        let elapsed_str = if elapsed_ms < 1000 {
            format!("{elapsed_ms}ms")
        } else {
            format!("{}", HumanDuration(discovery_elapsed))
        };
        println!(
            "  {} {} files, hashed in {}",
            style("Discovered").dim(),
            events.len(),
            elapsed_str,
        );
    } else {
        println!(
            "  {} {} files (hashing skipped)",
            style("Discovered").dim(),
            events.len(),
        );
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

    let pb = ProgressBar::new(events.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}",
        )
        .expect("valid progress template")
        .progress_chars("#>-"),
    );
    pb.set_message("Probing...");

    let intro_start = Instant::now();
    let mut introspected = 0u64;
    let mut errors = 0u64;
    let total = events.len() as u64;

    for (i, event) in events.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        let eta = format_eta(intro_start.elapsed(), i + 1, total as usize);
        let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len() + 9); // "Probing "
        let name = event
            .path
            .file_name()
            .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
            .unwrap_or_default();

        pb.set_message(format!("Probing {name}{eta}"));

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

        pb.inc(1);
    }

    pb.finish_and_clear();

    let total_elapsed = start.elapsed();
    if token.is_cancelled() {
        println!(
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
        return Ok(());
    }

    println!(
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

    if args.table {
        let results: Vec<_> = events
            .iter()
            .map(|e| (e.path.clone(), e.size, e.content_hash.clone()))
            .collect();
        output::format_scan_results(&results, crate::cli::OutputFormat::Table);
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
        f.content_hash = "abc123".into();
        f
    }

    #[tokio::test]
    async fn test_events_dispatched_through_kernel() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        // Simulate discovery event
        let discovered =
            FileDiscoveredEvent::new(PathBuf::from("/tmp/test.mkv"), 1024, "abc123".into());
        kernel.dispatch(Event::FileDiscovered(discovered));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);

        // Simulate introspection event
        let file = test_media_file("/tmp/test.mkv");
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent::new(file)));

        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_format_eta_zero_current_returns_empty() {
        assert_eq!(format_eta(Duration::from_secs(1), 0, 100), "");
    }

    #[test]
    fn test_format_eta_complete_returns_empty() {
        // When current == total, remaining == 0 so empty string
        assert_eq!(format_eta(Duration::from_secs(1), 100, 100), "");
    }

    #[test]
    fn test_format_eta_in_progress_returns_nonempty() {
        let eta = format_eta(Duration::from_secs(1), 1, 100);
        // Should produce something like ", ETA 1s" or similar
        assert!(eta.starts_with(", ETA "), "expected ETA prefix, got: {eta}");
    }

    #[tokio::test]
    async fn test_multiple_discovery_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        let events = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 100, "aaa".into()),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/b.mp4"), 200, "bbb".into()),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/c.avi"), 300, "ccc".into()),
        ];

        for event in &events {
            kernel.dispatch(Event::FileDiscovered(event.clone()));
        }

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 3);
    }
}
