//! End-to-end test: run a `voom scan` in-process against a tiny fixture and
//! verify the `plugin_stats` table is populated (issue #92).
//!
//! These tests exercise the full pipeline:
//!   bus dispatcher → StatsSink → SqliteStatsSink writer thread → SQLite
//!   → rollup_plugin_stats → voom plugin stats

#![cfg(feature = "functional")]

mod common;

use std::time::Duration;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use voom_domain::plugin_stats::PluginStatsFilter;
use voom_domain::storage::PluginStatsStorage;
use voom_sqlite_store::store::SqliteStore;

use common::{EnvOverride, TestEnv};

// All tests in this file manipulate `XDG_CONFIG_HOME` via std::env::set_var.
// That is not safe to do concurrently. Serialize with this mutex.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run a `voom scan` in-process for the given `TestEnv`, with `XDG_CONFIG_HOME`
/// pointed at the env's isolated config dir.
///
/// The `XDG_CONFIG_HOME` override is held via a RAII guard so that even if any
/// `.expect()` / `panic!()` below trips, the env var is restored before the
/// next test runs.
async fn run_scan(env: &TestEnv) {
    let config_home = env.config_home().to_str().expect("utf-8 config_home");
    let _guard = EnvOverride::set("XDG_CONFIG_HOME", config_home);

    let root_str = env.root().to_str().expect("utf-8 root");
    let argv = ["voom", "scan", "--no-hash", root_str];
    let cli = voom_cli::cli::Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("CLI parse failed: {e}"));
    let scan_args = match cli.command {
        voom_cli::cli::Commands::Scan(a) => a,
        _ => panic!("expected Scan subcommand"),
    };

    let token = CancellationToken::new();
    voom_cli::commands::scan::run(scan_args, true, token)
        .await
        .unwrap_or_else(|e| panic!("voom scan failed: {e}"));
}

/// Open a read-only handle to the env's SQLite DB for post-run queries.
fn open_store(env: &TestEnv) -> std::sync::Arc<SqliteStore> {
    let db_path = env.db_path();
    let store = SqliteStore::open(&db_path)
        .unwrap_or_else(|e| panic!("failed to open store at {}: {e}", db_path.display()));
    std::sync::Arc::new(store)
}

// ─── Test 1 ──────────────────────────────────────────────────────────────────

/// A `voom scan` must record at least one row in `plugin_stats`, and at least
/// one of those rows must come from the `discovery` plugin.
#[tokio::test]
async fn scan_populates_plugin_stats_table() {
    let _lock = TEST_LOCK.lock().await;
    let env = TestEnv::new().await;

    // A non-empty file is sufficient to trigger the discovery plugin's emit path.
    // Introspection may fail (it's not a real MKV), but that failure is itself
    // an invocation record and counts toward the row total.
    env.write_media("a.mkv", 32);

    run_scan(&env).await;

    // Allow the writer thread up to 2 s to flush batched rows.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let store = open_store(&env);
    let filter = PluginStatsFilter::new(None, None, None);
    let rollups = store
        .rollup_plugin_stats(&filter)
        .expect("rollup_plugin_stats should succeed");

    assert!(
        !rollups.is_empty(),
        "plugin_stats rollup should have at least one row after a scan; got zero"
    );

    // Direct SQL assertions that specific named plugins appear in the table.
    //
    // `sqlite-store` is a bus subscriber: `Bus::publish_recursive` records the
    // *subscriber's* plugin_id on every event it handles, so a single scan
    // produces many `sqlite-store` rows. This invariant predates Phase 3.
    //
    // `discovery` is a capability-dispatched plugin: after Phase 3 (#378), the
    // CLI scan command issues one `Call::ScanLibrary` per root through
    // `Kernel::dispatch_to_capability`, which records a `PluginStatRecord` for
    // the resolved plugin (see `voom-kernel/src/lib.rs` ~lines 497-505). So at
    // least one `discovery` row must appear after any successful scan.
    let db_path = env.db_path();
    let conn = rusqlite::Connection::open(&db_path)
        .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
    let store_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plugin_stats WHERE plugin_id = 'sqlite-store'",
            [],
            |r| r.get(0),
        )
        .expect("count sqlite-store rows");
    assert!(
        store_count > 0,
        "expected at least one sqlite-store row in plugin_stats"
    );

    let discovery_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plugin_stats WHERE plugin_id = 'discovery'",
            [],
            |r| r.get(0),
        )
        .expect("count discovery rows");
    assert!(
        discovery_count > 0,
        "expected at least one 'discovery' row in plugin_stats after a scan; the CLI dispatches \
         ScanLibrary through Kernel::dispatch_to_capability which records the resolved plugin id \
         (#378 Phase 3)"
    );
}

// ─── Test 2 ──────────────────────────────────────────────────────────────────

/// After a `voom scan`, calling the `voom plugin stats --format json` CLI
/// handler in-process must run to completion without error.
///
/// We don't capture stdout — the rendered contents are exercised by Test 1's
/// direct DB assertion. What this test contributes is end-to-end coverage of
/// the handler's `args → filter → rollup → JSON serialization → stdout` path.
#[tokio::test]
async fn plugin_stats_handler_runs_after_scan() {
    let _lock = TEST_LOCK.lock().await;
    let env = TestEnv::new().await;
    env.write_media("a.mkv", 1);

    run_scan(&env).await;

    tokio::time::sleep(Duration::from_millis(2000)).await;

    // The handler reloads config via `config::load_config()`, so we need
    // `XDG_CONFIG_HOME` still pointed at the test env's config dir while it runs.
    let config_home = env.config_home().to_str().expect("utf-8 config_home");
    let _guard = EnvOverride::set("XDG_CONFIG_HOME", config_home);

    let result = voom_cli::commands::plugin_stats::run(
        None,                              // plugin filter
        None,                              // since
        None,                              // top
        voom_cli::cli::OutputFormat::Json, // format
    );
    assert!(
        result.is_ok(),
        "voom plugin stats --format json must succeed end-to-end, got: {:?}",
        result.err()
    );
}

// ─── Test 3 ──────────────────────────────────────────────────────────────────

/// After a scan populates `plugin_stats` and `event_log`, invoking
/// `plugin_stats::run` must NOT add new rows to either table. The command is
/// read-only; bootstrapping a full kernel just to render a rollup is the bug
/// this test guards against (Codex adversarial review, May 2026).
#[tokio::test]
async fn plugin_stats_query_does_not_mutate_database() {
    let _lock = TEST_LOCK.lock().await;
    let env = TestEnv::new().await;
    env.write_media("a.mkv", 1);

    run_scan(&env).await;

    // Allow the writer thread to flush.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let db_path = env.db_path();
    let baseline_plugin_stats: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap()
    };
    let baseline_event_log: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0))
            .unwrap()
    };
    assert!(baseline_plugin_stats > 0, "scan must produce stats first");

    // Run the stats query the same way the CLI handler does.
    let config_home = env.config_home().to_str().expect("utf-8 config_home");
    let _guard = EnvOverride::set("XDG_CONFIG_HOME", config_home);

    voom_cli::commands::plugin_stats::run(None, None, None, voom_cli::cli::OutputFormat::Json)
        .expect("plugin stats query must succeed");

    drop(_guard);

    // Allow any pending writer-thread work (there should be none) to settle.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let after_plugin_stats: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap()
    };
    let after_event_log: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0))
            .unwrap()
    };

    assert_eq!(
        after_plugin_stats, baseline_plugin_stats,
        "plugin_stats query must not add plugin_stats rows: was {baseline_plugin_stats}, now \
         {after_plugin_stats}"
    );
    assert_eq!(
        after_event_log, baseline_event_log,
        "plugin_stats query must not add event_log rows: was {baseline_event_log}, now \
         {after_event_log}"
    );
}

// ─── Test 4 (issue #382) ─────────────────────────────────────────────────────

/// `voom estimate calibrate` must NOT add rows to `plugin_stats` or
/// `event_log`. It only writes `cost_model_samples`; using
/// `bootstrap_kernel_with_store` for this would register every plugin
/// and dispatch their init events through the bus, polluting the
/// bookkeeping tables — the same anti-pattern that was fixed for
/// `voom plugin stats` in de69c75.
#[tokio::test]
async fn estimate_calibrate_does_not_mutate_database() {
    use clap::Parser;

    let _lock = TEST_LOCK.lock().await;
    let env = TestEnv::new().await;
    // Seed the DB by running a scan first, so plugin_stats and
    // event_log are non-empty before we measure deltas.
    env.write_media("a.mkv", 1);
    run_scan(&env).await;
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let db_path = env.db_path();
    let baseline_plugin_stats: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap()
    };
    let baseline_event_log: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0))
            .unwrap()
    };
    let baseline_cost_model_samples: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM cost_model_samples", [], |r| r.get(0))
            .unwrap()
    };
    assert!(
        baseline_plugin_stats > 0,
        "scan must produce stats first; got {baseline_plugin_stats}"
    );

    // Run `voom estimate calibrate` end-to-end in-process.
    let config_home = env.config_home().to_str().expect("utf-8 config_home");
    let _guard = EnvOverride::set("XDG_CONFIG_HOME", config_home);
    let argv = ["voom", "estimate", "calibrate"];
    let cli = voom_cli::cli::Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("CLI parse failed: {e}"));
    let estimate_args = match cli.command {
        voom_cli::cli::Commands::Estimate(a) => a,
        _ => panic!("expected Estimate subcommand"),
    };
    let token = tokio_util::sync::CancellationToken::new();
    voom_cli::commands::estimate::run(estimate_args, true, token)
        .await
        .expect("voom estimate calibrate must succeed");
    drop(_guard);

    // Settle any (theoretically nonexistent) pending writer work.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let after_plugin_stats: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap()
    };
    let after_event_log: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM event_log", [], |r| r.get(0))
            .unwrap()
    };
    let after_cost_model_samples: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM cost_model_samples", [], |r| r.get(0))
            .unwrap()
    };

    assert_eq!(
        after_plugin_stats, baseline_plugin_stats,
        "estimate calibrate must not add plugin_stats rows: was {baseline_plugin_stats}, \
         now {after_plugin_stats}"
    );
    assert_eq!(
        after_event_log, baseline_event_log,
        "estimate calibrate must not add event_log rows: was {baseline_event_log}, now \
         {after_event_log}"
    );
    assert!(
        after_cost_model_samples > baseline_cost_model_samples,
        "estimate calibrate MUST add cost_model_samples rows: was \
         {baseline_cost_model_samples}, now {after_cost_model_samples}"
    );
}

// ─── Phase 4: process-path stats coverage ────────────────────────────────────

/// A `voom process --dry-run` must record at least one `policy-evaluator` row
/// in `plugin_stats`. `--dry-run` exercises the full process pipeline up to
/// (but not including) executor mutation, so the per-phase `kernel_invoke::evaluate`
/// call site fires and the kernel's dispatch_to_capability instrumentation
/// writes a `plugin_stats` row whose `plugin_id = "policy-evaluator"`.
///
/// This is the regression that protects the spec acceptance criterion
/// "`plugin_stats` contains rows for `discovery`, `policy-evaluator`, and
/// `phase-orchestrator` after a `voom process` run" (#378).
#[tokio::test]
async fn process_dry_run_populates_policy_evaluator_stats_row() {
    let _lock = TEST_LOCK.lock().await;
    let mut env = TestEnv::new().await;

    // A one-phase policy that evaluates against any container — keeps the
    // evaluator's work non-trivial without depending on real ffprobe output.
    env.set_policy(
        r#"policy "phase4-stats-fixture" {
            phase init { container mkv }
        }"#,
    );
    env.write_media("a.mkv", 32);

    let policy_path = env
        .policy_path()
        .to_str()
        .expect("utf-8 policy path")
        .to_string();
    let root = env.root().to_str().expect("utf-8 root").to_string();

    let _outcome = env
        .run_process(&[
            &root,
            "--policy",
            &policy_path,
            "--dry-run",
            "--no-backup",
        ])
        .await;

    // Allow the SqliteStatsSink writer thread to flush batched rows.
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let db_path = env.db_path();
    let conn = rusqlite::Connection::open(&db_path)
        .unwrap_or_else(|e| panic!("open {}: {e}", db_path.display()));
    let evaluator_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plugin_stats WHERE plugin_id = 'policy-evaluator'",
            [],
            |r| r.get(0),
        )
        .expect("count policy-evaluator rows");
    assert!(
        evaluator_count > 0,
        "expected at least one 'policy-evaluator' row in plugin_stats after a \
         dry-run process; the CLI dispatches Call::EvaluatePolicy through \
         Kernel::dispatch_to_capability which records the resolved plugin id \
         (#378 Phase 4). Got {evaluator_count} rows."
    );
}
