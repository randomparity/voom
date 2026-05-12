//! Unified progress indicators for CLI commands.
//!
//! Provides three concrete types that standardize progress display:
//! - [`DiscoveryProgress`] — spinner that transitions to a bar (discovery → hashing)
//! - [`ProbeProgress`] — determinate bar for introspection loops
//! - [`BatchProgress`] — determinate bar implementing [`ProgressReporter`]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use console::style;
use indicatif::{HumanDuration, ProgressBar, ProgressStyle};
use parking_lot::Mutex;

use crate::output::{PROGRESS_FIXED_WIDTH, max_filename_len, shrink_filename};
use voom_domain::events::FileDiscoveredEvent;
use voom_job_manager::progress::ProgressReporter;

const TICK_INTERVAL: Duration = Duration::from_millis(80);
const SPINNER_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
const BAR_CHARS: &str = "#>-";
const BAR_TEMPLATE: &str = "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%) {msg}";
const SPINNER_TEMPLATE: &str = "{spinner:.green} {msg}";
const MIN_JOB_WEIGHT: f64 = 1.0;
const ETA_WARMUP_JOBS: usize = 2;
const ETA_WARMUP_ELAPSED: Duration = Duration::from_secs(10);
const ETA_PROGRESS_CAP: f64 = 0.98;
const ETA_SMOOTHING_ALPHA: f64 = 0.35;
const ETA_MAX_STEP_UP: f64 = 1.30;
const ETA_MAX_STEP_DOWN: f64 = 0.75;

/// Format an ETA string from elapsed time and progress counts.
///
/// Returns `, ETA {duration}` for uniform appending after any message.
/// Returns an empty string when ETA cannot be meaningfully computed.
pub fn format_eta(elapsed: Duration, current: usize, total: usize) -> String {
    if current == 0 {
        return String::new();
    }
    // File counts and seconds stay well under 2^52, so the f64 cast is safe
    // for the rate/ETA arithmetic that follows.
    #[allow(clippy::cast_precision_loss)]
    let rate = current as f64 / elapsed.as_secs_f64();
    #[allow(clippy::cast_precision_loss)]
    let remaining = (total - current) as f64 / rate;
    if remaining.is_finite() && remaining > 0.0 {
        // `remaining` is positive and finite here, and ETAs beyond u64 seconds
        // are not meaningful — saturate to avoid wrap.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let secs = remaining as u64;
        format!(", ETA {}", HumanDuration(Duration::from_secs(secs)))
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

    /// Create a hidden (no-op) progress indicator for quiet/scripted mode.
    pub fn hidden() -> Self {
        Self {
            pb: ProgressBar::hidden(),
            start: Instant::now(),
            transitioned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Like [`Self::new`] but attached to a shared `MultiProgress` so the
    /// bar coexists with other bars (e.g. the probe bar in streaming mode).
    pub fn new_in(mp: &indicatif::MultiProgress) -> Self {
        let pb = mp.add(ProgressBar::new_spinner());
        pb.set_style(spinner_style());
        pb.enable_steady_tick(TICK_INTERVAL);
        Self {
            pb,
            start: Instant::now(),
            transitioned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Reset from bar back to spinner for a new discovery phase.
    ///
    /// Called between directories so the progress bar doesn't show stale
    /// position/length from the previous directory's processing phase.
    pub fn reset_to_spinner(&self) {
        if self.transitioned.swap(false, Ordering::Relaxed) {
            self.pb.set_style(spinner_style());
            self.pb.set_position(0);
            self.pb.set_length(0);
            self.pb.set_message("Discovering...");
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
    /// Always updates bar length to support cumulative totals across
    /// multiple scan directories.
    pub fn on_processing(&self, current: usize, total: usize, path: &Path, action: &str) {
        // Relaxed is sufficient: a duplicate set_style call is harmless (ProgressBar is thread-safe).
        if !self.transitioned.swap(true, Ordering::Relaxed) {
            self.pb.set_style(bar_style());
        }
        self.pb.set_length(total as u64);
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
#[derive(Clone)]
pub struct ProbeProgress {
    pb: ProgressBar,
    start: Instant,
    total: usize,
}

impl ProbeProgress {
    /// Create a probe progress bar with an *initial* total of zero.
    /// The total is grown via [`Self::add_pending`] as work is enqueued.
    /// Suitable for streaming mode where the caller doesn't know the final
    /// total until discovery completes.
    ///
    /// Production code uses [`Self::new_dynamic_in`] (attached to a
    /// `MultiProgress`). This variant is used in tests and standalone contexts.
    #[allow(dead_code)] // used in tests; production callers use new_dynamic_in
    pub fn new_dynamic() -> Self {
        let pb = ProgressBar::new(0);
        pb.set_style(bar_style());
        pb.enable_steady_tick(TICK_INTERVAL);
        pb.set_message("Probing...");
        Self {
            pb,
            start: Instant::now(),
            total: 0,
        }
    }

    /// Hidden no-op equivalent of [`Self::new_dynamic`].
    pub fn hidden_dynamic() -> Self {
        Self {
            pb: ProgressBar::hidden(),
            start: Instant::now(),
            total: 0,
        }
    }

    /// Like [`Self::new_dynamic`] but attached to a shared `MultiProgress`.
    pub fn new_dynamic_in(mp: &indicatif::MultiProgress) -> Self {
        let pb = mp.add(ProgressBar::new(0));
        pb.set_style(bar_style());
        pb.enable_steady_tick(TICK_INTERVAL);
        pb.set_message("Probing...");
        Self {
            pb,
            start: Instant::now(),
            total: 0,
        }
    }

    /// Increase the bar's total by `n`. Safe to call from any thread.
    pub fn add_pending(&self, n: u64) {
        self.pb.inc_length(n);
    }

    /// Current bar length (for tests).
    #[cfg(test)]
    pub(crate) fn length(&self) -> u64 {
        self.pb.length().unwrap_or(0)
    }

    /// Current bar position (for tests).
    #[cfg(test)]
    pub(crate) fn position(&self) -> u64 {
        self.pb.position()
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
    state: Mutex<BatchEtaState>,
}

impl BatchProgress {
    /// Create a new batch processing progress bar.
    pub fn new(files: &[FileDiscoveredEvent], workers: usize) -> Self {
        let total = files.len();
        let overall = ProgressBar::new(total as u64);
        overall.set_style(bar_style());
        overall.enable_steady_tick(TICK_INTERVAL);
        Self {
            overall,
            state: Mutex::new(BatchEtaState::new(files, workers)),
        }
    }

    /// Create a hidden (no-op) progress bar for quiet/scripted mode.
    pub fn hidden(files: &[FileDiscoveredEvent], workers: usize) -> Self {
        let overall = ProgressBar::hidden();
        overall.set_length(files.len() as u64);
        Self {
            overall,
            state: Mutex::new(BatchEtaState::new(files, workers)),
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
                let eta = {
                    let mut state = self.state.lock();
                    state.on_job_start(job.id, &payload)
                };
                let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
                let name = truncated_filename(std::path::Path::new(&payload.path), max_name);
                self.overall.set_message(format!("{name}{eta}"));
            }
        }
    }

    fn on_job_progress(&self, id: uuid::Uuid, progress: f64, _msg: Option<&str>) {
        let eta = {
            let mut state = self.state.lock();
            state.on_job_progress(id, progress)
        };
        if let Some((path, eta)) = eta {
            let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
            let name = truncated_filename(std::path::Path::new(&path), max_name);
            self.overall.set_message(format!("{name}{eta}"));
        }
    }

    fn on_job_complete(&self, id: uuid::Uuid, _success: bool, error: Option<&str>) {
        if let Some(err) = error {
            self.overall.suspend(|| {
                eprintln!("{} {err}", style("ERROR:").bold().red());
            });
        }
        let eta = {
            let mut state = self.state.lock();
            state.on_job_complete(id)
        };
        self.overall.inc(1);
        if let Some((path, eta)) = eta {
            let max_name = max_filename_len(PROGRESS_FIXED_WIDTH + eta.len());
            let name = truncated_filename(std::path::Path::new(&path), max_name);
            self.overall.set_message(format!("{name}{eta}"));
        }
    }

    fn on_batch_complete(&self, _completed: u64, _failed: u64) {
        self.overall.finish_and_clear();
    }

    fn on_jobs_extended(&self, additional: usize) {
        let new_len = self.overall.length().unwrap_or(0) + additional as u64;
        self.overall.set_length(new_len);
    }

    fn seed_events(&self, events: &[voom_domain::events::FileDiscoveredEvent]) {
        // Streaming mode: the pipeline calls this once, right before
        // `on_batch_start`, with the full list of files that will be processed.
        // Replace the internal ETA state with one seeded from those events so
        // size-based ETA estimates work normally during the determinate phase.
        let workers = {
            let state = self.state.lock();
            state.effective_workers
        };
        let new_state = BatchEtaState::new(events, workers);
        *self.state.lock() = new_state;
    }
}

#[derive(Clone, Debug)]
struct RunningJobState {
    path: String,
    weight: f64,
    started_at: Instant,
    progress: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EtaConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug)]
struct EtaEstimate {
    eta: Option<Duration>,
    // `confidence` and `remaining_work` are populated for introspection in
    // tests and future UI surfaces; they are not read by the current ETA
    // formatter, which renders only `eta`.
    #[allow(dead_code)]
    confidence: EtaConfidence,
    #[allow(dead_code)]
    remaining_work: f64,
}

#[derive(Debug)]
struct BatchEtaState {
    batch_started_at: Instant,
    queued_weights: HashMap<String, f64>,
    running_jobs: HashMap<uuid::Uuid, RunningJobState>,
    completed_job_durations: Vec<Duration>,
    completed_weight: f64,
    completed_jobs: usize,
    effective_workers: usize,
    displayed_eta_secs: Option<f64>,
    current_path: Option<String>,
}

impl BatchEtaState {
    fn new(files: &[FileDiscoveredEvent], workers: usize) -> Self {
        let mut queued_weights = HashMap::with_capacity(files.len());
        for file in files {
            let path = file.path.to_string_lossy().into_owned();
            let weight = job_weight(file.size);
            queued_weights.insert(path, weight);
        }

        Self {
            batch_started_at: Instant::now(),
            queued_weights,
            running_jobs: HashMap::new(),
            completed_job_durations: Vec::new(),
            completed_weight: 0.0,
            completed_jobs: 0,
            effective_workers: workers.max(1),
            displayed_eta_secs: None,
            current_path: None,
        }
    }

    fn on_job_start(
        &mut self,
        job_id: uuid::Uuid,
        payload: &crate::introspect::DiscoveredFilePayload,
    ) -> String {
        let weight = self
            .queued_weights
            .remove(&payload.path)
            .unwrap_or_else(|| job_weight(payload.size));
        self.current_path = Some(payload.path.clone());
        self.running_jobs.insert(
            job_id,
            RunningJobState {
                path: payload.path.clone(),
                weight,
                started_at: Instant::now(),
                progress: None,
            },
        );
        self.eta_string()
    }

    fn on_job_progress(&mut self, job_id: uuid::Uuid, progress: f64) -> Option<(String, String)> {
        let running = self.running_jobs.get_mut(&job_id)?;
        running.progress = Some(progress.clamp(0.0, 1.0));
        self.current_path = Some(running.path.clone());
        Some((running.path.clone(), self.eta_string()))
    }

    fn on_job_complete(&mut self, job_id: uuid::Uuid) -> Option<(String, String)> {
        let finished = self.running_jobs.remove(&job_id)?;
        let elapsed = finished.started_at.elapsed();
        self.completed_weight += finished.weight;
        self.completed_jobs += 1;
        self.completed_job_durations.push(elapsed);
        self.current_path = self
            .running_jobs
            .values()
            .next()
            .map(|job| job.path.clone())
            .or_else(|| self.current_path.clone());
        self.current_path
            .clone()
            .map(|path| (path, self.eta_string()))
    }

    fn eta_string(&mut self) -> String {
        let estimate = self.estimate();
        match estimate.eta {
            Some(eta) if eta > Duration::ZERO => format!(", ETA {}", HumanDuration(eta)),
            _ => String::new(),
        }
    }

    fn estimate(&mut self) -> EtaEstimate {
        let remaining_work = self.remaining_weight();
        if remaining_work <= 0.0 {
            self.displayed_eta_secs = None;
            return EtaEstimate {
                eta: None,
                confidence: EtaConfidence::High,
                remaining_work: 0.0,
            };
        }

        let elapsed = self.batch_started_at.elapsed();
        if self.completed_jobs < ETA_WARMUP_JOBS && elapsed < ETA_WARMUP_ELAPSED {
            return EtaEstimate {
                eta: None,
                confidence: EtaConfidence::Low,
                remaining_work,
            };
        }

        let seconds_per_weight = match self.seconds_per_weight() {
            Some(value) if value.is_finite() && value > 0.0 => value,
            _ => {
                return EtaEstimate {
                    eta: None,
                    confidence: EtaConfidence::Low,
                    remaining_work,
                };
            }
        };

        let mut loads: Vec<f64> = self
            .running_jobs
            .values()
            .map(|job| self.predicted_remaining_runtime_secs(job, seconds_per_weight))
            .collect();

        if loads.len() < self.effective_workers {
            loads.resize(self.effective_workers, 0.0);
        }

        let mut queued_durations: Vec<f64> = self
            .queued_weights
            .values()
            .map(|weight| weight * seconds_per_weight)
            .filter(|duration| duration.is_finite() && *duration > 0.0)
            .collect();
        queued_durations.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        for duration in queued_durations {
            if let Some((slot, _)) = loads
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            {
                loads[slot] += duration;
            }
        }

        let raw_eta_secs = loads.into_iter().fold(0.0_f64, f64::max);
        let eta = self.smooth_eta(raw_eta_secs);
        let confidence = if self.completed_jobs >= ETA_WARMUP_JOBS * 3 {
            EtaConfidence::High
        } else {
            EtaConfidence::Medium
        };
        EtaEstimate {
            eta: duration_from_secs(eta),
            confidence,
            remaining_work,
        }
    }

    fn seconds_per_weight(&self) -> Option<f64> {
        let mut observed_weight = self.completed_weight;
        let mut observed_runtime_secs: f64 = self
            .completed_job_durations
            .iter()
            .map(Duration::as_secs_f64)
            .sum();

        if observed_weight <= 0.0 {
            return None;
        }

        for running in self.running_jobs.values() {
            if let Some(progress) = self.progress_for_job(running, None) {
                if progress > 0.0 {
                    observed_weight += running.weight * progress;
                    observed_runtime_secs += running.started_at.elapsed().as_secs_f64();
                }
            }
        }

        if observed_runtime_secs <= 0.0 || observed_weight <= 0.0 {
            None
        } else {
            Some(observed_runtime_secs / observed_weight)
        }
    }

    fn predicted_remaining_runtime_secs(
        &self,
        job: &RunningJobState,
        seconds_per_weight: f64,
    ) -> f64 {
        let elapsed = job.started_at.elapsed().as_secs_f64();
        let expected_total = (job.weight * seconds_per_weight).max(elapsed);

        if let Some(progress) = self.progress_for_job(job, Some(seconds_per_weight)) {
            if progress > 0.0 {
                let inferred_total = (elapsed / progress).max(expected_total);
                return (inferred_total - elapsed).max(0.0);
            }
        }

        (expected_total - elapsed).max(0.0)
    }

    // `&self` is kept so callers use method syntax consistently with the
    // other progress-tracking helpers in this impl; splitting it out as an
    // associated function would churn every call site.
    #[allow(clippy::unused_self)]
    fn progress_for_job(
        &self,
        job: &RunningJobState,
        seconds_per_weight: Option<f64>,
    ) -> Option<f64> {
        if let Some(progress) = job.progress {
            return Some(progress.clamp(0.0, ETA_PROGRESS_CAP));
        }

        let seconds_per_weight = seconds_per_weight?;
        let expected_total = job.weight * seconds_per_weight;
        if expected_total <= 0.0 {
            return None;
        }

        Some((job.started_at.elapsed().as_secs_f64() / expected_total).clamp(0.0, ETA_PROGRESS_CAP))
    }

    fn remaining_weight(&self) -> f64 {
        let in_flight_weight: f64 = self
            .running_jobs
            .values()
            .map(|job| {
                let progress = self
                    .progress_for_job(job, self.seconds_per_weight())
                    .unwrap_or(0.0);
                job.weight * (1.0 - progress)
            })
            .sum();
        let queued_weight: f64 = self.queued_weights.values().sum();
        in_flight_weight + queued_weight
    }

    fn smooth_eta(&mut self, raw_eta_secs: f64) -> f64 {
        if !raw_eta_secs.is_finite() || raw_eta_secs <= 0.0 {
            self.displayed_eta_secs = None;
            return 0.0;
        }

        let next = match self.displayed_eta_secs {
            Some(previous) => {
                let smoothed = previous + ETA_SMOOTHING_ALPHA * (raw_eta_secs - previous);
                smoothed.clamp(previous * ETA_MAX_STEP_DOWN, previous * ETA_MAX_STEP_UP)
            }
            None => raw_eta_secs,
        };
        self.displayed_eta_secs = Some(next);
        next
    }
}

fn job_weight(size: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)] // file sizes beyond 2^52 bytes would exceed disk capacity
    let size = size as f64;
    size.max(MIN_JOB_WEIGHT)
}

fn duration_from_secs(secs: f64) -> Option<Duration> {
    if secs.is_finite() && secs > 0.0 {
        Some(Duration::from_secs_f64(secs))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let result = truncated_filename(path, 20);
        assert!(result.len() <= 20);
        assert!(result.ends_with("...mkv"), "got: {result}");
    }

    #[test]
    fn test_truncated_filename_short_enough() {
        let path = Path::new("/dir/short.mkv");
        assert_eq!(truncated_filename(path, 40), "short.mkv");
    }

    #[test]
    fn test_truncated_filename_no_filename() {
        let path = Path::new("/");
        assert_eq!(truncated_filename(path, 40), "");
    }

    #[test]
    fn test_batch_eta_state_waits_for_warmup() {
        let files = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 100, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/b.mkv"), 100, None),
        ];
        let mut state = BatchEtaState::new(&files, 1);
        let job_id = uuid::Uuid::new_v4();
        let payload = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/a.mkv".into(),
            size: 100,
            content_hash: None,
        };
        assert_eq!(state.on_job_start(job_id, &payload), "");
    }

    #[test]
    fn test_batch_eta_state_accounts_for_queued_heavy_tail() {
        let files = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/small-1.mkv"), 1, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/small-2.mkv"), 1, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/large.mkv"), 100, None),
        ];
        let mut state = BatchEtaState::new(&files, 1);
        state.batch_started_at = Instant::now().checked_sub(Duration::from_secs(12)).unwrap();

        let first = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/small-1.mkv".into(),
            size: 1,
            content_hash: None,
        };
        let second = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/small-2.mkv".into(),
            size: 1,
            content_hash: None,
        };
        state.on_job_start(uuid::Uuid::new_v4(), &first);
        let first_id = *state.running_jobs.keys().next().unwrap();
        let start = state.running_jobs.get_mut(&first_id).unwrap();
        start.started_at = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        state.on_job_complete(first_id);

        state.on_job_start(uuid::Uuid::new_v4(), &second);
        let second_id = *state.running_jobs.keys().next().unwrap();
        let start = state.running_jobs.get_mut(&second_id).unwrap();
        start.started_at = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        state.on_job_complete(second_id);

        let estimate = state.estimate();
        assert!(estimate.eta.unwrap_or_default() >= Duration::from_secs(50));
    }

    #[test]
    fn test_probe_progress_dynamic_total_starts_at_zero() {
        let pb = ProbeProgress::new_dynamic();
        assert_eq!(pb.length(), 0);
    }

    #[test]
    fn test_probe_progress_add_pending_grows_total() {
        let pb = ProbeProgress::new_dynamic();
        pb.add_pending(3);
        assert_eq!(pb.length(), 3);
        pb.add_pending(2);
        assert_eq!(pb.length(), 5);
    }

    #[test]
    fn test_probe_progress_inc_position() {
        let pb = ProbeProgress::new_dynamic();
        pb.add_pending(2);
        pb.inc();
        assert_eq!(pb.position(), 1);
    }

    #[test]
    fn test_batch_eta_state_accounts_for_concurrency() {
        let files = vec![
            FileDiscoveredEvent::new(PathBuf::from("/tmp/a.mkv"), 10, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/b.mkv"), 10, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/c.mkv"), 10, None),
            FileDiscoveredEvent::new(PathBuf::from("/tmp/d.mkv"), 10, None),
        ];
        let mut state = BatchEtaState::new(&files, 2);
        state.batch_started_at = Instant::now().checked_sub(Duration::from_secs(20)).unwrap();

        let payload_a = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/a.mkv".into(),
            size: 10,
            content_hash: None,
        };
        let payload_b = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/b.mkv".into(),
            size: 10,
            content_hash: None,
        };

        state.on_job_start(uuid::Uuid::new_v4(), &payload_a);
        let first_id = state
            .running_jobs
            .iter()
            .find(|(_, job)| job.path == "/tmp/a.mkv")
            .map(|(id, _)| *id)
            .unwrap();
        state.running_jobs.get_mut(&first_id).unwrap().started_at =
            Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        state.on_job_complete(first_id);

        state.on_job_start(uuid::Uuid::new_v4(), &payload_b);
        let second_id = state
            .running_jobs
            .iter()
            .find(|(_, job)| job.path == "/tmp/b.mkv")
            .map(|(id, _)| *id)
            .unwrap();
        state.running_jobs.get_mut(&second_id).unwrap().started_at =
            Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        state.on_job_complete(second_id);

        let payload_c = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/c.mkv".into(),
            size: 10,
            content_hash: None,
        };
        let payload_d = crate::introspect::DiscoveredFilePayload {
            path: "/tmp/d.mkv".into(),
            size: 10,
            content_hash: None,
        };
        state.on_job_start(uuid::Uuid::new_v4(), &payload_c);
        state.on_job_start(uuid::Uuid::new_v4(), &payload_d);
        for running in state.running_jobs.values_mut() {
            running.started_at = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        }

        let estimate = state.estimate();
        assert!(estimate.eta.unwrap_or_default() < Duration::from_secs(15));
    }

    #[test]
    fn batch_progress_seed_events_rebuilds_eta_state() {
        use std::path::PathBuf;
        use voom_domain::events::FileDiscoveredEvent;

        // Start with zero events (the streaming-mode default).
        let bp = BatchProgress::hidden(&[], 2);

        // Seed three events after the fact.
        let events: Vec<FileDiscoveredEvent> = (0..3)
            .map(|i| {
                FileDiscoveredEvent::new(
                    PathBuf::from(format!("/tmp/f{i}.mkv")),
                    1024 * (i + 1),
                    None,
                )
            })
            .collect();
        bp.seed_events(&events);

        // After seeding, the state must contain three queued weights.
        let state = bp.state.lock();
        assert_eq!(state.queued_weights.len(), 3);
        assert_eq!(state.effective_workers, 2);
    }

    #[test]
    fn batch_progress_on_jobs_extended_extends_total() {
        let bp = BatchProgress::hidden(&[], 1);
        // Length starts at zero in streaming mode.
        assert_eq!(bp.overall.length(), Some(0));
        bp.on_jobs_extended(5);
        assert_eq!(bp.overall.length(), Some(5));
        bp.on_jobs_extended(3);
        assert_eq!(bp.overall.length(), Some(8));
    }
}
