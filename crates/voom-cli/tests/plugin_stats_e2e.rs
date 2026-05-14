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
    let filter = PluginStatsFilter {
        plugin: None,
        since: None,
        top: None,
    };
    let rollups = store
        .rollup_plugin_stats(&filter)
        .expect("rollup_plugin_stats should succeed");

    assert!(
        !rollups.is_empty(),
        "plugin_stats rollup should have at least one row after a scan; got zero"
    );

    // Direct SQL assertion that a specific named plugin appears in the table.
    // The dispatcher records the *subscriber's* plugin_id, not the publisher's
    // (see `Bus::publish_recursive` in voom-kernel/src/bus.rs). Discovery is a
    // pure publisher (`handles()` returns false for every event — see
    // `DiscoveryPlugin::test_handles_no_events`), so it never appears in
    // `plugin_stats`. We assert on `sqlite-store` instead, which subscribes to
    // every event for persistence and is therefore the canonical reliable
    // subscriber on any scan run.
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
