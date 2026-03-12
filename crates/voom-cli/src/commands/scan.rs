use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;
use voom_domain::events::Event;

use crate::app;
use crate::cli::ScanArgs;
use crate::output::{self, max_filename_len, shrink_filename};

/// Fixed-width overhead of the progress bar line (spinner + bar + counters + percent + padding).
const PROGRESS_FIXED_WIDTH: usize = 77;

/// Format an ETA string from elapsed time and progress counts.
/// Returns an empty string when ETA cannot be meaningfully computed.
fn format_eta(start: &Instant, current: usize, total: usize) -> String {
    if current == 0 {
        return String::new();
    }
    let elapsed = start.elapsed().as_secs_f64();
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
/// so that subscribers (sqlite-store, SSE, WASM plugins) receive them.
pub async fn run(args: ScanArgs) -> Result<()> {
    let config = app::load_config()?;
    let kernel = app::bootstrap_kernel(&config)?;

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
                    let eta = format_eta(&start, current, total);
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
        .map_err(|e| anyhow::anyhow!("filesystem scan failed: {e}"))?;

    pb.finish_and_clear();

    if events.is_empty() {
        println!("{}", "No media files found.".yellow());
        return Ok(());
    }

    // Publish discovery events through the event bus
    for event in &events {
        kernel.dispatch(Event::FileDiscovered(event.clone()));
    }

    // Introspect each discovered file
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
        let eta = format_eta(&intro_start, i + 1, total as usize);
        let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
        let name = event
            .path
            .file_name()
            .map(|n| shrink_filename(&n.to_string_lossy(), max_name))
            .unwrap_or_default();

        pb.set_message(format!("{}{}", name, eta));

        let path = event.path.clone();
        let size = event.size;
        let hash = event.content_hash.clone();
        let intro_result = tokio::task::spawn_blocking(move || {
            let introspector = voom_ffprobe_introspector::FfprobeIntrospectorPlugin::new();
            introspector.introspect(&path, size, &hash)
        })
        .await;
        match intro_result {
            Ok(Ok(intro_event)) => {
                kernel.dispatch(Event::FileIntrospected(intro_event));
                introspected += 1;
            }
            Ok(Err(e)) => {
                tracing::warn!(path = %event.path.display(), error = %e, "introspection failed");
                errors += 1;
            }
            Err(e) => {
                tracing::warn!(path = %event.path.display(), error = %e, "introspection task panicked");
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
            matches!(event_type, "file.discovered" | "file.introspected")
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
        let discovered = FileDiscoveredEvent {
            path: PathBuf::from("/tmp/test.mkv"),
            size: 1024,
            content_hash: "abc123".into(),
        };
        kernel.dispatch(Event::FileDiscovered(discovered));

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 1);

        // Simulate introspection event
        let file = test_media_file("/tmp/test.mkv");
        kernel.dispatch(Event::FileIntrospected(FileIntrospectedEvent { file }));

        assert_eq!(recorder.introspected_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_multiple_discovery_events_dispatched() {
        let mut kernel = voom_kernel::Kernel::new();
        let recorder = Arc::new(RecordingPlugin::new());
        kernel.register_plugin(recorder.clone(), 50);

        let events = vec![
            FileDiscoveredEvent {
                path: PathBuf::from("/tmp/a.mkv"),
                size: 100,
                content_hash: "aaa".into(),
            },
            FileDiscoveredEvent {
                path: PathBuf::from("/tmp/b.mp4"),
                size: 200,
                content_hash: "bbb".into(),
            },
            FileDiscoveredEvent {
                path: PathBuf::from("/tmp/c.avi"),
                size: 300,
                content_hash: "ccc".into(),
            },
        ];

        for event in &events {
            kernel.dispatch(Event::FileDiscovered(event.clone()));
        }

        assert_eq!(recorder.discovered_count.load(Ordering::SeqCst), 3);
    }
}
