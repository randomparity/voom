//! Unified progress indicators for CLI commands.
//!
//! Provides three concrete types that standardize progress display:
//! - [`DiscoveryProgress`] — spinner that transitions to a bar (discovery → hashing)
//! - [`ProbeProgress`] — determinate bar for introspection loops
//! - [`BatchProgress`] — determinate bar implementing [`ProgressReporter`]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use console::style;
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};

use crate::output::{max_filename_len, shrink_filename, PROGRESS_FIXED_WIDTH};
use voom_job_manager::progress::ProgressReporter;

const TICK_INTERVAL: Duration = Duration::from_millis(80);
const SPINNER_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
const BAR_CHARS: &str = "#>-";
const BAR_TEMPLATE: &str = "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}";
const SPINNER_TEMPLATE: &str = "{spinner:.green} {msg}";

/// Format an ETA string from elapsed time and progress counts.
///
/// Returns `, ETA {duration}` for uniform appending after any message.
/// Returns an empty string when ETA cannot be meaningfully computed.
pub fn format_eta(elapsed: Duration, current: usize, total: usize) -> String {
    if current == 0 {
        return String::new();
    }
    let rate = current as f64 / elapsed.as_secs_f64();
    let remaining = (total - current) as f64 / rate;
    if remaining.is_finite() && remaining > 0.0 {
        format!(
            ", ETA {}",
            HumanDuration(Duration::from_secs(remaining as u64))
        )
    } else {
        String::new()
    }
}

/// Extract a filename from a path and truncate it for progress display.
fn truncated_filename(path: &Path, max_len: usize) -> String {
    path.file_name()
        .map(|n| shrink_filename(&n.to_string_lossy(), max_len))
        .unwrap_or_default()
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template(SPINNER_TEMPLATE)
        .expect("valid progress template")
        .tick_chars(SPINNER_CHARS)
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(BAR_TEMPLATE)
        .expect("valid progress template")
        .progress_chars(BAR_CHARS)
}

/// Progress indicator for file discovery that transitions from spinner to bar.
///
/// Starts as an indeterminate spinner during directory walking, then
/// switches to a determinate bar when processing (hashing/scanning) begins.
#[derive(Clone)]
pub struct DiscoveryProgress {
    pb: ProgressBar,
    start: Instant,
    transitioned: Arc<AtomicBool>,
}

impl DiscoveryProgress {
    /// Create a new discovery-phase progress indicator.
    pub fn new() -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(spinner_style());
        pb.enable_steady_tick(TICK_INTERVAL);
        Self {
            pb,
            start: Instant::now(),
            transitioned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Update for a newly discovered file.
    pub fn on_discovered(&self, count: usize, path: &Path) {
        let prefix = format!("Discovering... {count} files found \u{2014} ");
        let max_name = max_filename_len(2 + prefix.len());
        let name = truncated_filename(path, max_name);
        self.pb.set_message(format!("{prefix}{name}"));
    }

    /// Update for a processing step (hashing/scanning).
    ///
    /// Transitions from spinner to determinate bar on the first call.
    pub fn on_processing(&self, current: usize, total: usize, path: &Path, action: &str) {
        // Relaxed is sufficient: a duplicate set_style call is harmless (ProgressBar is thread-safe).
        if !self.transitioned.swap(true, Ordering::Relaxed) {
            self.pb.set_length(total as u64);
            self.pb.set_style(bar_style());
        }
        let eta = format_eta(self.start.elapsed(), current, total);
        let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + action.len() + 1 + eta.len());
        let name = truncated_filename(path, max_name);
        self.pb.set_position(current as u64);
        self.pb.set_message(format!("{action} {name}{eta}"));
    }

    /// Finish and clear the progress bar.
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

/// Determinate progress bar for introspection (probing) loops.
pub struct ProbeProgress {
    pb: ProgressBar,
    start: Instant,
    total: usize,
}

impl ProbeProgress {
    /// Create a new introspection progress bar.
    pub fn new(total: usize) -> Self {
        let pb = ProgressBar::new(total as u64);
        pb.set_style(bar_style());
        pb.enable_steady_tick(TICK_INTERVAL);
        pb.set_message("Probing...");
        Self {
            pb,
            start: Instant::now(),
            total,
        }
    }

    /// Update progress for the current file being probed.
    pub fn on_file(&self, index: usize, path: &Path) {
        let eta = format_eta(self.start.elapsed(), index, self.total);
        let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + "Probing ".len() + eta.len());
        let name = truncated_filename(path, max_name);
        self.pb.set_message(format!("Probing {name}{eta}"));
    }

    /// Increment the position by one.
    pub fn inc(&self) {
        self.pb.inc(1);
    }

    /// Finish and clear the progress bar.
    pub fn finish(&self) {
        self.pb.finish_and_clear();
    }
}

/// Determinate progress bar for batch processing via the worker pool.
///
/// Implements [`ProgressReporter`] so it can be composed with
/// [`CompositeReporter`](voom_job_manager::progress::CompositeReporter).
pub struct BatchProgress {
    overall: ProgressBar,
    start: Instant,
    total: u64,
}

impl BatchProgress {
    /// Create a new batch processing progress bar.
    pub fn new(total: usize) -> Self {
        let overall = ProgressBar::new(total as u64);
        overall.set_style(bar_style());
        overall.enable_steady_tick(TICK_INTERVAL);
        Self {
            overall,
            start: Instant::now(),
            total: total as u64,
        }
    }

    fn eta_string(&self) -> String {
        let pos = self.overall.position();
        if pos == 0 {
            return String::new();
        }
        let rate = pos as f64 / self.start.elapsed().as_secs_f64();
        let remaining = (self.total - pos) as f64 / rate;
        if remaining.is_finite() && remaining > 0.0 {
            format!(
                ", ETA {}",
                HumanDuration(Duration::from_secs(remaining as u64))
            )
        } else {
            String::new()
        }
    }
}

impl ProgressReporter for BatchProgress {
    fn on_batch_start(&self, _total: usize) {}

    fn on_job_start(&self, job: &voom_domain::job::Job) {
        if let Some(ref raw) = job.payload {
            if let Ok(payload) =
                serde_json::from_value::<crate::introspect::DiscoveredFilePayload>(raw.clone())
            {
                let eta = self.eta_string();
                let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
                let name = truncated_filename(std::path::Path::new(&payload.path), max_name);
                self.overall.set_message(format!("{name}{eta}"));
            }
        }
    }

    fn on_job_progress(&self, _id: uuid::Uuid, _progress: f64, _msg: Option<&str>) {}

    fn on_job_complete(&self, _id: uuid::Uuid, _success: bool, error: Option<&str>) {
        if let Some(err) = error {
            self.overall.suspend(|| {
                eprintln!("{} {err}", style("ERROR:").bold().red());
            });
        }
        self.overall.inc(1);
    }

    fn on_batch_complete(&self, _completed: u64, _failed: u64) {
        self.overall.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_eta_zero_current_returns_empty() {
        assert_eq!(format_eta(Duration::from_secs(1), 0, 100), "");
    }

    #[test]
    fn test_format_eta_complete_returns_empty() {
        assert_eq!(format_eta(Duration::from_secs(1), 100, 100), "");
    }

    #[test]
    fn test_format_eta_in_progress_returns_prefixed() {
        let eta = format_eta(Duration::from_secs(1), 1, 100);
        assert!(
            eta.starts_with(", ETA "),
            "expected ', ETA ' prefix, got: {eta}"
        );
    }

    #[test]
    fn test_truncated_filename_with_extension() {
        let path = Path::new("/some/dir/A Very Long Movie Name (2025).mkv");
        let result = truncated_filename(&path, 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("...mkv"), "got: {result}");
    }

    #[test]
    fn test_truncated_filename_short_enough() {
        let path = Path::new("/dir/short.mkv");
        assert_eq!(truncated_filename(&path, 40), "short.mkv");
    }

    #[test]
    fn test_truncated_filename_no_filename() {
        let path = Path::new("/");
        assert_eq!(truncated_filename(&path, 40), "");
    }
}
