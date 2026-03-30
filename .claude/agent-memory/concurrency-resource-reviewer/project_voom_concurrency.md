---
name: VOOM concurrency architecture
description: Key concurrency patterns, lock hierarchy, and resource management strategies found in the VOOM codebase
type: project
---

# VOOM concurrency architecture (updated feat/address-cli-gaps-1, 2026-03-29)

**Why:** Serves as an institutional knowledge base for concurrency reviews. Captures the topology so future reviews know where to look and what invariants to verify.

**How to apply:** Use as a starting point for any concurrency audit, to understand the runtime layout, lock hierarchy, and known contention points.

## Runtime topology

- **tokio runtime**: single multi-thread runtime started by `voom-cli/src/main.rs` via `#[tokio::main]`. No explicit `worker_threads` configuration — defaults to num CPUs.
- **rayon**: used by `plugins/discovery/src/scanner.rs` for parallel file walking (`walkdir` + rayon `par_iter`). Rayon has its own global thread pool (or per-scan custom pool when `workers > 0`), correctly separate from tokio.
- **std::thread**: used by `crates/voom-process/src/lib.rs::spawn_pipe_readers` for stdout/stderr draining — two OS threads per subprocess invocation. These are detached from both tokio and rayon.
- **bootstrap_kernel_with_store** (`crates/voom-cli/src/app.rs`) is called synchronously at startup — before the tokio main runs any async code. Plugin `init()` methods run here: they call `Command::new("ffmpeg")`, `Command::new("mkvmerge")`, `std::fs::write` (health probe) — all blocking I/O. This is safe as of today because bootstrap runs synchronously before the runtime, but is brittle if ever called from an async context.

## Lock inventory

- `EventBus.subscribers: parking_lot::RwLock<Vec<Subscriber>>` — held briefly to collect handlers, released before dispatch. Read lock in `publish_recursive`, write lock in `subscribe_plugin`. Safe pattern.
- `Kernel.shutdown_called: AtomicBool` — CAS to prevent double-shutdown. Correct.
- `CapabilityCollectorPlugin.map: std::sync::Mutex<CapabilityMap>` — held only inside synchronous `on_event`, never across await. Safe.
- `BusTracerPlugin.writer: Option<Arc<parking_lot::Mutex<File>>>` — held only inside synchronous `on_event` for file write, never across await. Safe.
- `RunCounters.phase_stats: Arc<parking_lot::Mutex<HashMap<...>>>` — acquired inside `process_single_file` (tokio task). Held briefly for stat updates; no I/O inside lock. Safe.
- `RunCounters.plan_collector: Arc<parking_lot::Mutex<Vec<...>>>` — acquired inside `process_single_file` (tokio task). Held briefly; no I/O inside lock. Safe.
- **No tokio::sync::Mutex found anywhere.**

## SQLite / r2d2

- Pool size: 8 connections (`SqliteStoreConfig::pool_size = 8`).
- WAL mode: confirmed in `plugins/sqlite-store/src/schema.rs::configure_connection` via `PRAGMA journal_mode = WAL`.
- Busy timeout: confirmed, 5000ms via `PRAGMA busy_timeout = 5000`.
- `conn()` returns `r2d2::PooledConnection` — RAII returned on drop. Connections are never held across await points (the plugin's `on_event` is synchronous).
- `health_checks` table has `prune_health_checks` wired to the serve background task (`serve.rs` line 72) on a ~daily schedule. The prune is correctly wrapped in `spawn_blocking`.

## Job manager

- Semaphore: `tokio::sync::Semaphore` with `effective_workers` permits (default = num CPUs).
- All SQLite calls in `worker.rs` (`claim_by_id`, `complete`, `fail`, `cancel`) are correctly wrapped in `tokio::task::spawn_blocking`.
- Cancellation: `tokio_util::sync::CancellationToken` — cooperative, checked before claim, after claim, and before each plan in `execute_plans`.
- `StorageReporter.on_job_progress` calls `store.update_job` (blocking) directly — this reporter is only composed in daemon scenarios, not in CLI `process` runs.
- `EventBusReporter.on_job_progress` (in `process.rs:949`) calls `self.kernel.dispatch(Event::JobProgress(...))`. This triggers `sqlite-store.on_event` → `store.insert_event_log` synchronously inside a tokio task. This is a blocking-on-tokio issue.

## Process command critical path

- `process_single_file` runs inside `tokio::spawn` (via `WorkerPool::process_batch`).
- `introspect_file` correctly uses `spawn_blocking` for ffprobe execution.
- `execute_plans` correctly uses `spawn_blocking` for plan execution (`execute_single_plan`).
- Hash check (`voom_discovery::hash_file`) is correctly wrapped in `spawn_blocking`.
- Post-execution re-introspection correctly uses `spawn_blocking` + `introspect_file`.
- **Blocking concern**: `ctx.counters.phase_stats.lock()` and `plan_collector.lock()` are `parking_lot::Mutex` calls on the tokio runtime without `spawn_blocking`. These locks are brief (no I/O inside) so this is acceptable in practice, but parking_lot locks are not yield-point-aware.

## Serve command

- Health checker background task (`serve.rs` lines 59-95): correctly uses `tokio::task::spawn_blocking` for both the `prune_health_checks` call and the `checker.run_checks` + `k.dispatch` sequence.
- Previous confirmed issue (P1 — blocking health dispatch without spawn_blocking) is now **RESOLVED**.

## Rate limiter (web-server)

- `RateLimitLayer`: two `Arc<RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>>` — sharded, lock-free for most operations.
- Layer ordering: `RateLimitLayer` applied after `ConcurrencyLimitLayer` (tower inside-out evaluation). Rate check fires after concurrency limit. Inefficient but not incorrect.
- IPv6-mapped IPv4 (e.g. `::ffff:1.2.3.4`) creates separate rate buckets from the same physical IPv4 client. Low practical risk on LAN.

## External process lifecycle

- `voom_process::run_with_timeout_env`: on timeout, calls `child.kill()` then `child.wait()` to reap. No zombie processes.
- stdout/stderr draining threads are joined after wait. On timeout path, `join_pipe_readers` is called after `child.wait()` — the threads complete because the process is killed and pipes close. Correct.
- No cancellation signal propagated from `CancellationToken` into the subprocess while it's running (the 4-hour ffmpeg timeout is the only kill mechanism). Long transcodes cannot be soft-cancelled mid-execution.

## Discovery

- rayon custom thread pool created per-scan when `workers > 0`. Rayon pool drops at end of scan. No leak.
- `on_progress` callback is called from rayon threads — the callback (in `process.rs`) only updates indicatif progress bars. indicatif is thread-safe.
- No file handle limits enforced — for enormous libraries, rayon will open many files simultaneously during hashing. OS limit is the backstop.

## Known confirmed issues (cumulative, as of 2026-03-29)

1. **P1 (pre-existing, still present)**: `StorageReporter::on_job_progress` calls `store.update_job` (blocking SQLite) from inside a tokio worker task without `spawn_blocking`. Only triggered in daemon mode when `StorageReporter` is composed in.
2. **P1 (new this review)**: `EventBusReporter::on_job_progress` calls `kernel.dispatch(Event::JobProgress(...))` from inside a tokio task — this triggers `sqlite-store.on_event` → `insert_event_log` (blocking SQLite write) on the tokio runtime.
3. **P2**: `health_checks` table pruning is wired but only runs ~daily; very high-frequency health check schedules can still grow the table between prunes.
4. **Info**: `RateLimitLayer` ordering (fires after concurrency limit is consumed).
5. **Info**: No file handle concurrency limit during rayon hashing for very large libraries.
6. **Info**: Long-running subprocess (ffmpeg transcode) cannot be cooperatively cancelled — `CancellationToken` is checked between plans, not inside the blocking subprocess execution.
7. **Fixed (previously P1)**: Periodic health check dispatch in `serve.rs` now correctly uses `spawn_blocking`.
