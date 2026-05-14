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

use common::TestEnv;

// All tests in this file manipulate `XDG_CONFIG_HOME` via std::env::set_var.
// That is not safe to do concurrently. Serialize with this mutex.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run a `voom scan` in-process for the given `TestEnv`, with `XDG_CONFIG_HOME`
/// pointed at the env's isolated config dir.
///
/// Returns once the scan completes. The caller is responsible for waiting an
/// additional moment for the stats-sink writer thread to flush.
async fn run_scan(env: &TestEnv) {
    // Point config loading at our isolated tempdir so `bootstrap_kernel_with_store`
    // uses the test env's data_dir rather than the user's real config.
    let config_home = env.config_home().to_str().expect("utf-8 config_home");
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    // SAFETY: tests are serialized by TEST_LOCK; see struct-level note in common/mod.rs.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", config_home);
    }

    let root_str = env.root().to_str().expect("utf-8 root");
    let argv = ["voom", "scan", "--no-hash", root_str];
    let cli = voom_cli::cli::Cli::try_parse_from(argv)
        .unwrap_or_else(|e| panic!("CLI parse failed: {e}"));
    let scan_args = match cli.command {
        voom_cli::cli::Commands::Scan(a) => a,
        _ => panic!("expected Scan subcommand"),
    };

    let token = CancellationToken::new();
    let result = voom_cli::commands::scan::run(scan_args, true, token).await;

    // Restore the prior env var before asserting so cleanup happens even on panic.
    #[allow(unused_unsafe)]
    unsafe {
        match &prior {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }

    result.unwrap_or_else(|e| panic!("voom scan failed: {e}"));
}

/// Open a read-only handle to the env's SQLite DB for post-run queries.
fn open_store(env: &TestEnv) -> std::sync::Arc<SqliteStore> {
    let db_path = env.db_path();
    let store = SqliteStore::open(&db_path)
        .unwrap_or_else(|e| panic!("failed to open store at {}: {e}", db_path.display()));
    std::sync::Arc::new(store)
}

// ─── Test 1 ──────────────────────────────────────────────────────────────────

/// A `voom scan` must record at least one row in `plugin_stats`.
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
}

// ─── Test 2 ──────────────────────────────────────────────────────────────────

/// After a `voom scan`, the `rollup_plugin_stats` query must return a non-empty
/// slice — the same data that `voom plugin stats` renders.
///
/// This mirrors the behaviour the CLI exercises: it calls
/// `bootstrap_kernel_with_store` → `rollup_plugin_stats`. We test the same
/// path without spawning an external binary.
#[tokio::test]
async fn plugin_stats_rollup_non_empty_after_scan() {
    let _lock = TEST_LOCK.lock().await;
    let env = TestEnv::new().await;
    env.write_media("a.mkv", 1);

    run_scan(&env).await;

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
        "expected at least one rollup row; the stats sink pipeline may not be wired correctly"
    );

    // Verify the rollup shape: every row must have a non-empty plugin_id and a
    // non-negative invocation count.
    for r in &rollups {
        assert!(
            !r.plugin_id.is_empty(),
            "plugin_id must be non-empty; got {:?}",
            r
        );
        assert!(
            r.invocation_count > 0,
            "invocation_count must be > 0; got {:?}",
            r
        );
    }
}
