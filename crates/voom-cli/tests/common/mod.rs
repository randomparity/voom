//! Shared harness for phase-5 acceptance tests.
//!
//! ## Quick-start
//!
//! ```no_run
//! # async fn _doc() {
//! use common::{TestEnv, ProcessOutcome};
//! let mut env = TestEnv::new().await;
//! env.write_media("movie.mkv", 1024);
//! let outcome = env.run_process(&["process", env.root().to_str().unwrap(), "--dry-run"]).await;
//! assert!(outcome.result.is_ok());
//! # }
//! ```
//!
//! ## Notes on the event timeline
//!
//! `EventTimeline` is built from the SQLite `event_log` table after the run
//! completes. Timestamps are converted to monotonic `Instant` values using
//! the baseline `Instant` recorded just before `process::run` is invoked:
//!
//! ```text
//! instant = run_start_instant + (event.created_at - run_start_utc)
//! ```
//!
//! Events that were recorded before `run_start_utc` (unlikely but possible if
//! the DB already had rows) are clamped to `run_start_instant`.

// This module is test infrastructure. Items are used by acceptance test files
// that `mod common;` into their compilation unit. Suppress dead-code lints that
// fire when no acceptance test has been written yet.
#![allow(dead_code, unused_imports, clippy::type_complexity)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use clap::Parser;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use voom_domain::DiscoveredFilePayload;
use voom_domain::storage::{EventLogFilters, JobFilters, JobStorage, StorageTrait};
use voom_sqlite_store::store::SqliteStore;

/// Default no-op policy used when the caller does not call `set_policy`.
const DEFAULT_POLICY: &str = r#"policy "test" {
    phase noop {
        keep audio
    }
}
"#;

/// A single row from the `jobs` table captured during polling.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: uuid::Uuid,
    pub path: PathBuf,
    pub state: String,
    pub priority: i32,
}

/// Ordered sequence of `(wall-clock Instant, Event)` pairs collected after
/// the run by reading the SQLite `event_log` table.
pub struct EventTimeline {
    entries: Vec<(Instant, voom_domain::Event)>,
}

impl EventTimeline {
    fn new(entries: Vec<(Instant, voom_domain::Event)>) -> Self {
        Self { entries }
    }

    /// Return the timestamp of the first event matching `pred`, or `None`.
    pub fn first_at(&self, pred: impl Fn(&voom_domain::Event) -> bool) -> Option<Instant> {
        self.entries
            .iter()
            .find(|(_, ev)| pred(ev))
            .map(|(ts, _)| *ts)
    }

    /// Return all recorded `(Instant, Event)` pairs in log order.
    pub fn all(&self) -> &[(Instant, voom_domain::Event)] {
        &self.entries
    }

    /// Return the `Instant` at which `RootWalkCompleted` was emitted for
    /// `root`, or `None`.
    pub fn root_walk_completed_at(&self, root: &Path) -> Option<Instant> {
        self.first_at(|ev| {
            if let voom_domain::Event::RootWalkCompleted(e) = ev {
                e.root == root
            } else {
                false
            }
        })
    }

    /// Return the `Instant` at which `PlanExecuting` was emitted for the
    /// file at `path`, or `None`.
    pub fn plan_execution_started_at(&self, path: &Path) -> Option<Instant> {
        self.first_at(|ev| {
            if let voom_domain::Event::PlanExecuting(e) = ev {
                e.path == path
            } else {
                false
            }
        })
    }
}

/// Outcome returned by `TestEnv::run_process`.
pub struct ProcessOutcome {
    /// The `Result` returned by `commands::process::run`.
    pub result: anyhow::Result<()>,
    /// Event timeline reconstructed from the SQLite `event_log` table.
    pub events: EventTimeline,
    /// Handle to the SQLite store for post-run queries.
    pub store: Arc<dyn StorageTrait>,
    /// Periodic snapshots of the `jobs` table taken every ~25 ms during the run.
    pub sql_job_snapshots: Vec<(Instant, Vec<JobRow>)>,
}

impl ProcessOutcome {
    /// Return the snapshot whose timestamp is nearest to and ≤ `ts`.
    pub fn sql_jobs_at(&self, ts: Instant) -> Option<&[JobRow]> {
        self.sql_job_snapshots
            .iter()
            .rev()
            .find(|(snapshot_ts, _)| *snapshot_ts <= ts)
            .map(|(_, rows)| rows.as_slice())
    }
}

/// Isolated test environment. Each `TestEnv` gets its own tempdir, config,
/// data dir, and policy file so tests do not share state.
pub struct TestEnv {
    /// Root tempdir. Dropped (deleted) when `TestEnv` is dropped.
    pub tmp: tempfile::TempDir,
    /// The primary scan root (`<tmp>/media`). Also available via `root()`.
    primary_root: PathBuf,
    /// All registered roots (`name → path`).
    roots: HashMap<String, PathBuf>,
    /// VOOM data directory (`<tmp>/data`). The SQLite DB lives here.
    data_dir: PathBuf,
    /// `XDG_CONFIG_HOME` override (`<tmp>/config`).
    config_home: PathBuf,
    /// Content of the `.voom` policy file.
    policy_text: String,
    /// Path to the written `.voom` policy file.
    policy_path: PathBuf,
    /// Per-root delays injected via the `test-hooks` feature.
    #[cfg_attr(not(feature = "test-hooks"), allow(dead_code))]
    root_delays: HashMap<PathBuf, Duration>,
}

impl TestEnv {
    /// Create a fresh isolated environment with an in-process SQLite store,
    /// a default no-op policy, and a single `media/` root.
    pub async fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create TestEnv tempdir");
        let data_dir = tmp.path().join("data");
        let config_home = tmp.path().join("config");
        let primary_root = tmp.path().join("media");
        let voom_config_dir = config_home.join("voom");

        std::fs::create_dir_all(&data_dir).expect("create data_dir");
        std::fs::create_dir_all(&primary_root).expect("create media root");
        std::fs::create_dir_all(&voom_config_dir).expect("create voom config dir");

        // Canonicalize paths so they match what `resolve_paths` produces in
        // process::run (which calls `Path::canonicalize` on all CLI args).
        // On macOS `/var` is a symlink to `/private/var`; without this step,
        // `RootWalkCompletedEvent.root` wouldn't match the path returned by
        // `add_root` and timing assertions would always fail.
        let data_dir = data_dir.canonicalize().expect("canonicalize data_dir");
        let primary_root = primary_root
            .canonicalize()
            .expect("canonicalize primary_root");
        let config_home = config_home
            .canonicalize()
            .expect("canonicalize config_home");

        // Write a config.toml that points data_dir to our tempdir.
        let config_toml = format!("data_dir = {:?}\n", data_dir.display().to_string());
        let config_toml_path = voom_config_dir.join("config.toml");
        std::fs::write(&config_toml_path, &config_toml).expect("write config.toml");

        // Restrict permissions so VOOM doesn't log "loose permissions" warnings.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_toml_path, std::fs::Permissions::from_mode(0o600))
                .ok();
        }

        // Write default policy.
        let policy_path = voom_config_dir.join("test.voom");
        std::fs::write(&policy_path, DEFAULT_POLICY).expect("write default policy");

        let mut roots = HashMap::new();
        roots.insert("media".to_string(), primary_root.clone());

        Self {
            tmp,
            primary_root,
            roots,
            data_dir,
            config_home,
            policy_text: DEFAULT_POLICY.to_string(),
            policy_path,
            root_delays: HashMap::new(),
        }
    }

    /// Add a named subdirectory inside the tempdir as an additional scan root.
    ///
    /// Returns the **canonical** path (resolving symlinks) so that it matches
    /// the paths stored in `RootWalkCompletedEvent.root` after `resolve_paths`
    /// canonicalizes the CLI args in `process::run`. On macOS `/var` is a
    /// symlink to `/private/var`; callers that use the returned path with
    /// `root_walk_completed_at` or `delay_root` must see the same canonical
    /// representation as the scanner.
    pub fn add_root(&mut self, name: &str) -> PathBuf {
        let path = self.tmp.path().join(name);
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("add_root {name}: {e}"));
        let canonical = path
            .canonicalize()
            .unwrap_or_else(|e| panic!("add_root {name}: canonicalize: {e}"));
        self.roots.insert(name.to_string(), canonical.clone());
        canonical
    }

    /// Return the primary scan root path.
    pub fn root(&self) -> &Path {
        &self.primary_root
    }

    /// Return the path to the SQLite database (`<data_dir>/voom.db`).
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("voom.db")
    }

    /// Return the config home directory (`<tmp>/config`).
    ///
    /// Set `XDG_CONFIG_HOME` to this path to isolate a scan run that calls
    /// `config::load_config` directly (e.g. `commands::scan::run`).
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// Write a synthetic media file at `<root>/<name>` with `size` bytes.
    ///
    /// Content is a repeating byte pattern derived from the file name so
    /// the xxHash is stable across runs.
    pub fn write_media_in(&self, root: &Path, name: &str, size: usize) {
        let path = root.join(name);
        let seed = name.as_bytes()[0];
        let data: Vec<u8> = (0..size).map(|i| seed.wrapping_add(i as u8)).collect();
        std::fs::write(&path, &data)
            .unwrap_or_else(|e| panic!("write_media_in {}: {e}", path.display()));
    }

    /// Write a synthetic media file with an explicit `mtime`.
    ///
    /// Useful for `--priority-by-date` tests where ordering depends on file
    /// modification time.
    pub fn write_media_in_with_mtime(
        &self,
        root: &Path,
        name: &str,
        size: usize,
        mtime: SystemTime,
    ) {
        self.write_media_in(root, name, size);
        let path = root.join(name);
        // Set mtime via filetime crate — but that's an extra dep. Instead use
        // std's set_modified if available (stabilized in Rust 1.75).
        #[cfg(unix)]
        {
            let secs = mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let tv = libc_timespec(secs);
            unsafe {
                let path_c = std::ffi::CString::new(path.to_str().expect("valid utf-8 path"))
                    .expect("CString");
                libc_utimensat(path_c.as_ptr(), &tv);
            }
        }
        #[cfg(not(unix))]
        {
            // Fallback: ignore mtime on non-Unix platforms. Tests that rely on
            // priority-by-date are Unix-only.
            let _ = mtime;
        }
    }

    /// Write a synthetic media file in the primary root.
    pub fn write_media(&self, name: &str, size: usize) {
        self.write_media_in(&self.primary_root, name, size);
    }

    /// Override the policy. `policy_text` must be a valid `.voom` policy
    /// string. Written to `<config>/test.voom`.
    pub fn set_policy(&mut self, policy_text: &str) {
        self.policy_text = policy_text.to_string();
        std::fs::write(&self.policy_path, policy_text).expect("write policy");
    }

    /// Return the path to the active `.voom` policy file.
    pub fn policy_path(&self) -> &Path {
        &self.policy_path
    }

    /// Inject a per-root scan delay.
    ///
    /// The delay fires inside `walk_media_files` before the directory walk
    /// begins, blocking the rayon worker assigned to that root. This makes
    /// walk completion time deterministic enough for ordering assertions.
    ///
    /// Only available with `--features test-hooks` (otherwise a no-op at
    /// compile time; the test file should be `#![cfg(feature = "test-hooks")]`).
    #[cfg(feature = "test-hooks")]
    pub fn delay_root(&mut self, root: &Path, delay: Duration) {
        self.root_delays.insert(root.to_path_buf(), delay);
    }

    /// Run `commands::process::run` in-process with the given extra CLI
    /// arguments prepended by `["voom", "process"]`.
    ///
    /// `args` should NOT include `voom` or `process` — those are added
    /// automatically. Example: `&["--dry-run", "/path/to/root"]`.
    pub async fn run_process(&self, args: &[&str]) -> ProcessOutcome {
        let token = CancellationToken::new();
        self.run_process_with_token(args, token).await
    }

    /// Same as `run_process` but with a caller-supplied `CancellationToken`.
    ///
    /// Pass a token whose `.cancel()` you control to test cancellation
    /// behaviour mid-run.
    pub async fn run_process_with_token(
        &self,
        args: &[&str],
        token: CancellationToken,
    ) -> ProcessOutcome {
        // Install test-hook delays before starting.
        #[cfg(feature = "test-hooks")]
        {
            voom_discovery::test_hooks::clear();
            for (root, delay) in &self.root_delays {
                voom_discovery::test_hooks::set_delay(root.clone(), *delay);
            }
        }

        // Build full argv: ["voom", "process", ...args]
        let mut argv: Vec<&str> = vec!["voom", "process"];
        argv.extend_from_slice(args);

        let cli = voom_cli::cli::Cli::try_parse_from(&argv)
            .unwrap_or_else(|e| panic!("TestEnv::run_process: CLI parse failed: {e}"));

        let process_args = match cli.command {
            voom_cli::cli::Commands::Process(a) => a,
            _ => panic!("expected Process subcommand (argv: {argv:?})"),
        };

        // Record baseline times for timestamp conversion.
        let run_start_instant = Instant::now();
        let run_start_utc = chrono::Utc::now();

        // Open a second pool-of-1 connection for background job polling.
        let db_path = self.data_dir.join("voom.db");
        let poll_store: Option<Arc<SqliteStore>> = SqliteStore::open(&db_path).ok().map(Arc::new);
        let snapshots: Arc<Mutex<Vec<(Instant, Vec<JobRow>)>>> = Arc::new(Mutex::new(Vec::new()));

        // Background job-snapshot task — polls every 25 ms until cancelled.
        let poll_token = CancellationToken::new();
        let poll_handle = if let Some(ps) = poll_store.clone() {
            let snaps = snapshots.clone();
            let pt = poll_token.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(25));
                loop {
                    tokio::select! {
                        () = pt.cancelled() => break,
                        _ = interval.tick() => {
                            let jobs = (*ps)
                                .list_jobs(&JobFilters::default())
                                .unwrap_or_default();
                            let rows: Vec<JobRow> = jobs
                                .iter()
                                .map(|j| JobRow {
                                    id: j.id,
                                    path: path_from_payload(j.payload.as_ref()),
                                    state: j.status.as_str().to_string(),
                                    priority: j.priority,
                                })
                                .collect();
                            let ts = Instant::now();
                            snaps.lock().await.push((ts, rows));
                        }
                    }
                }
            }))
        } else {
            None
        };

        // Point config loading at our isolated tempdir.
        let _config_guard =
            EnvOverride::set("XDG_CONFIG_HOME", self.config_home.to_str().expect("utf-8"));

        // Run process::run in the calling async context.
        let result = voom_cli::commands::process::run(process_args, true, token).await;

        // Stop the polling task.
        poll_token.cancel();
        if let Some(h) = poll_handle {
            h.await.ok();
        }

        // Clear test-hook delays.
        #[cfg(feature = "test-hooks")]
        voom_discovery::test_hooks::clear();

        // Open the store for the caller to query.
        let store: Arc<dyn StorageTrait> = Arc::new(
            SqliteStore::open(&db_path)
                .context("open store after run")
                .unwrap_or_else(|e| panic!("TestEnv: failed to open store after run: {e}")),
        );

        // Build event timeline from the event_log table.
        let timeline = build_timeline(&*store, run_start_instant, run_start_utc);

        let sql_job_snapshots = Arc::try_unwrap(snapshots)
            .expect("sole owner after polling task stopped")
            .into_inner();

        ProcessOutcome {
            result,
            events: timeline,
            store,
            sql_job_snapshots,
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build an `EventTimeline` from the SQLite `event_log` table.
///
/// Each row's `created_at` (`DateTime<Utc>`) is converted to an `Instant` by
/// computing the offset from `run_start_utc` and adding it to
/// `run_start_instant`. Events recorded before the run start are clamped.
fn build_timeline(
    store: &dyn StorageTrait,
    run_start_instant: Instant,
    run_start_utc: chrono::DateTime<chrono::Utc>,
) -> EventTimeline {
    let records = store
        .list_event_log(&EventLogFilters::default())
        .unwrap_or_default();

    let entries: Vec<(Instant, voom_domain::Event)> = records
        .into_iter()
        .filter_map(|rec| {
            // Deserialize the stored JSON payload back into an Event.
            let event: voom_domain::Event = serde_json::from_str(&rec.payload).ok()?;

            // Convert created_at to Instant via offset from run_start_utc.
            let offset = rec.created_at.signed_duration_since(run_start_utc);
            let ts = if offset.num_milliseconds() >= 0 {
                let ms = offset.num_milliseconds() as u64;
                run_start_instant + Duration::from_millis(ms)
            } else {
                // Event predates the run (e.g. pre-existing DB rows); clamp.
                run_start_instant
            };

            Some((ts, event))
        })
        .collect();

    EventTimeline::new(entries)
}

/// Extract the file path from a job's JSON payload.
///
/// Jobs created by the process pipeline carry a `DiscoveredFilePayload` with
/// a `"path"` field. Falls back to `PathBuf::new()` if the payload is absent
/// or the field is missing.
fn path_from_payload(payload: Option<&serde_json::Value>) -> PathBuf {
    payload
        .and_then(|v| serde_json::from_value::<DiscoveredFilePayload>(v.clone()).ok())
        .map(|p| PathBuf::from(p.path))
        .unwrap_or_default()
}

// ─── EnvOverride ────────────────────────────────────────────────────────────

/// RAII guard that sets an env var for the duration of its lifetime, then
/// restores the previous value (or removes it if it was previously absent).
///
/// Cheaply provides the `XDG_CONFIG_HOME` isolation needed by each test run
/// without requiring unsafe inter-thread mutations or test-serial sequencing.
///
/// **Note**: `std::env::set_var` is not thread-safe for concurrent use. Each
/// test must run with `--test-threads=1` or be wrapped in a per-test mutex.
/// The acceptance tests call `run_process` sequentially within a single async
/// test body, which is safe.
struct EnvOverride {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl EnvOverride {
    fn set(key: &'static str, value: &str) -> Self {
        let prior = std::env::var_os(key);
        // SAFETY: single-threaded test context; see struct-level note.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prior }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => {
                #[allow(unused_unsafe)]
                unsafe {
                    std::env::set_var(self.key, v);
                }
            }
            None => {
                #[allow(unused_unsafe)]
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }
}

// ─── Unix mtime helper ──────────────────────────────────────────────────────

#[cfg(unix)]
fn libc_timespec(secs: u64) -> [i64; 4] {
    // timespec pair for utimensat: [atime_sec, atime_nsec, mtime_sec, mtime_nsec]
    // We only care about mtime; pass UTIME_OMIT for atime.
    // UTIME_OMIT = 1073741822 on Linux/macOS.
    [1_073_741_822i64, 0, secs as i64, 0]
}

#[cfg(unix)]
unsafe fn libc_utimensat(path: *const i8, tv: &[i64; 4]) {
    // timespec is { tv_sec: i64, tv_nsec: i64 } on 64-bit platforms.
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }
    let ts = [
        Timespec {
            tv_sec: tv[0],
            tv_nsec: tv[1],
        },
        Timespec {
            tv_sec: tv[2],
            tv_nsec: tv[3],
        },
    ];
    unsafe extern "C" {
        fn utimensat(dirfd: i32, path: *const i8, times: *const Timespec, flags: i32) -> i32;
    }
    // AT_FDCWD = -100
    // SAFETY: path is a valid NUL-terminated C string; ts is a valid timespec pair.
    unsafe {
        utimensat(-100, path, ts.as_ptr(), 0);
    }
}
