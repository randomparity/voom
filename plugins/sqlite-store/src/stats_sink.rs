//! SQLite-backed StatsSink: non-blocking buffer + background writer thread.
//!
//! The bus dispatcher calls [`SqliteStatsSink::record`] from inside the
//! synchronous dispatch loop, which holds a `parking_lot::RwLock` read
//! lock on the subscriber list. The sink MUST NOT block. We use a bounded
//! `std::sync::mpsc::sync_channel` and `try_send`; on overflow, the record
//! is dropped (logged once at warn level).
//!
//! On shutdown, the writer drains the channel and retries the final flush
//! for up to [`SHUTDOWN_FLUSH_DEADLINE`]. Records that survive past the
//! deadline are accounted for in [`SqliteStatsSink::evicted_count`] and
//! logged at error level — never silently lost.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use voom_domain::plugin_stats::PluginStatRecord;
use voom_domain::storage::PluginStatsStorage;
use voom_kernel::stats_sink::StatsSink;

use crate::store::SqliteStore;

const DEFAULT_CHANNEL_CAPACITY: usize = 4096;
const DEFAULT_BATCH_SIZE: usize = 256;
const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(500);
/// Maximum consecutive failed flushes before the buffer is dropped as a last
/// resort to prevent unbounded memory growth. 30 attempts at 500ms ≈ 15s of
/// transient lock contention, which is well beyond normal SQLite back-off.
const MAX_CONSECUTIVE_FLUSH_FAILURES: u32 = 30;
/// Hard ceiling on the in-memory retry buffer. If the writer thread cannot
/// drain (e.g. DB completely unavailable), the oldest records are evicted
/// instead of growing the buffer without bound.
const MAX_BUFFER_RECORDS: usize = 64 * 1024;
/// Maximum time the writer will spend retrying the final flush at
/// shutdown. Tuned to ride out typical SQLite WAL lock contention while
/// keeping process shutdown bounded. Records that survive past the
/// deadline are evicted and the eviction is logged.
const SHUTDOWN_FLUSH_DEADLINE: Duration = Duration::from_secs(5);
/// Sleep between retry attempts during the shutdown drain.
const SHUTDOWN_RETRY_INTERVAL: Duration = Duration::from_millis(200);

pub struct SqliteStatsSink {
    tx: Option<SyncSender<PluginStatRecord>>,
    dropped: Arc<AtomicU64>,
    failed_flushes: Arc<AtomicU64>,
    evicted: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    writer: Option<JoinHandle<()>>,
}

impl SqliteStatsSink {
    /// Spawn a writer thread bound to `store`. The thread batches records
    /// up to `DEFAULT_BATCH_SIZE` or `DEFAULT_FLUSH_INTERVAL`, whichever
    /// comes first, and inserts via [`PluginStatsStorage::insert_plugin_stats_batch`].
    /// On insert failure the batch is RETAINED in the writer's buffer and
    /// retried on the next flush tick; records are only evicted when the
    /// buffer exceeds [`MAX_BUFFER_RECORDS`] or after
    /// [`MAX_CONSECUTIVE_FLUSH_FAILURES`] consecutive failures.
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        let (tx, rx) = sync_channel::<PluginStatRecord>(DEFAULT_CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));
        let failed_flushes = Arc::new(AtomicU64::new(0));
        let evicted = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let writer = std::thread::Builder::new()
            .name("voom-stats-writer".into())
            .spawn({
                let shutdown = shutdown.clone();
                let failed_flushes = failed_flushes.clone();
                let evicted = evicted.clone();
                move || writer_loop(rx, store, shutdown, failed_flushes, evicted)
            })
            .expect("spawn voom-stats-writer thread");
        Self {
            tx: Some(tx),
            dropped,
            failed_flushes,
            evicted,
            shutdown,
            writer: Some(writer),
        }
    }

    /// Records the dispatcher had to drop because the bounded channel was
    /// full or the writer thread had exited.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Number of times `insert_plugin_stats_batch` returned an error.
    /// The batch is retained for the next attempt; this counter goes up by
    /// one per failed attempt, not per record.
    #[must_use]
    pub fn failed_flush_count(&self) -> u64 {
        self.failed_flushes.load(Ordering::Relaxed)
    }

    /// Records evicted from the in-memory retry buffer to keep memory bounded
    /// (oldest-first, after `MAX_BUFFER_RECORDS` or
    /// `MAX_CONSECUTIVE_FLUSH_FAILURES`). Non-zero values mean the SQLite
    /// store has been unavailable long enough to lose data — alert worthy.
    #[must_use]
    pub fn evicted_count(&self) -> u64 {
        self.evicted.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
impl SqliteStatsSink {
    /// Test-only accessor that clones the evicted-counter Arc. Used by
    /// shutdown tests that need to observe the counter AFTER the sink is
    /// dropped (which moves the field).
    pub(crate) fn evicted_handle_for_tests(&self) -> Arc<AtomicU64> {
        self.evicted.clone()
    }

    /// Test-only constructor allowing a custom channel capacity to
    /// deterministically exercise the overflow path. `capacity=0` produces a
    /// rendezvous channel where every `try_send` fails unless the writer is
    /// actively in `recv`.
    pub(crate) fn with_capacity_for_tests(store: Arc<SqliteStore>, capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<PluginStatRecord>(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let failed_flushes = Arc::new(AtomicU64::new(0));
        let evicted = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let writer = std::thread::Builder::new()
            .name("voom-stats-writer-test".into())
            .spawn({
                let shutdown = shutdown.clone();
                let failed_flushes = failed_flushes.clone();
                let evicted = evicted.clone();
                move || writer_loop(rx, store, shutdown, failed_flushes, evicted)
            })
            .expect("spawn voom-stats-writer-test thread");
        Self {
            tx: Some(tx),
            dropped,
            failed_flushes,
            evicted,
            shutdown,
            writer: Some(writer),
        }
    }
}

impl StatsSink for SqliteStatsSink {
    fn record(&self, record: PluginStatRecord) {
        if let Some(tx) = &self.tx {
            if tx.try_send(record).is_err() {
                let prev = self.dropped.fetch_add(1, Ordering::Relaxed);
                if prev == 0 {
                    tracing::warn!(
                        "voom-stats-writer channel full or closed; dropping records (first occurrence)"
                    );
                }
            }
        }
    }
}

impl Drop for SqliteStatsSink {
    fn drop(&mut self) {
        // Setting shutdown is no longer strictly necessary — the writer's
        // Disconnected arm runs the final flush and exits — but leave it as
        // a safety net in case the writer is in the middle of a long flush
        // when we drop tx.
        self.shutdown.store(true, Ordering::Relaxed);
        // Drop the sender FIRST so the writer's recv_timeout returns
        // Err(Disconnected) on the next tick; the Disconnected arm drains
        // the channel and runs the guaranteed final flush before exiting.
        // Struct fields are dropped AFTER Drop::drop returns, so we must
        // explicitly take() and drop tx here, before joining the writer.
        drop(self.tx.take());
        if let Some(h) = self.writer.take() {
            let _ = h.join();
        }
    }
}

fn writer_loop(
    rx: Receiver<PluginStatRecord>,
    store: Arc<SqliteStore>,
    shutdown: Arc<AtomicBool>,
    failed_flushes: Arc<AtomicU64>,
    evicted: Arc<AtomicU64>,
) {
    let mut buf: Vec<PluginStatRecord> = Vec::with_capacity(DEFAULT_BATCH_SIZE);
    let mut last_flush = Instant::now();
    let mut consecutive_failures: u32 = 0;
    loop {
        match rx.recv_timeout(DEFAULT_FLUSH_INTERVAL) {
            Ok(record) => {
                buf.push(record);
                cap_buffer(&mut buf, &evicted);
                if buf.len() >= DEFAULT_BATCH_SIZE || last_flush.elapsed() >= DEFAULT_FLUSH_INTERVAL
                {
                    flush(
                        &store,
                        &mut buf,
                        &failed_flushes,
                        &evicted,
                        &mut consecutive_failures,
                    );
                    last_flush = Instant::now();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !buf.is_empty() {
                    flush(
                        &store,
                        &mut buf,
                        &failed_flushes,
                        &evicted,
                        &mut consecutive_failures,
                    );
                    last_flush = Instant::now();
                }
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Drain any remaining records from the closed channel so they are
                // included in the final flush attempts.
                while let Ok(record) = rx.try_recv() {
                    buf.push(record);
                    cap_buffer(&mut buf, &evicted);
                }
                if !buf.is_empty() {
                    let deadline = Instant::now() + SHUTDOWN_FLUSH_DEADLINE;
                    while !buf.is_empty() && Instant::now() < deadline {
                        flush(
                            &store,
                            &mut buf,
                            &failed_flushes,
                            &evicted,
                            &mut consecutive_failures,
                        );
                        if !buf.is_empty() {
                            std::thread::sleep(SHUTDOWN_RETRY_INTERVAL);
                        }
                    }
                    if !buf.is_empty() {
                        evicted.fetch_add(buf.len() as u64, Ordering::Relaxed);
                        tracing::error!(
                            dropped = buf.len(),
                            deadline_ms = u64::try_from(SHUTDOWN_FLUSH_DEADLINE.as_millis())
                                .unwrap_or(u64::MAX),
                            "plugin_stats writer abandoning records at shutdown after retry deadline; \
                             store unavailable"
                        );
                        buf.clear();
                    }
                }
                break;
            }
        }
    }
}

/// Drop oldest records when the retry buffer grows past `MAX_BUFFER_RECORDS`.
/// Accounted in `evicted` so callers can alert on data loss.
fn cap_buffer(buf: &mut Vec<PluginStatRecord>, evicted: &AtomicU64) {
    if buf.len() <= MAX_BUFFER_RECORDS {
        return;
    }
    let overflow = buf.len() - MAX_BUFFER_RECORDS;
    buf.drain(..overflow);
    evicted.fetch_add(overflow as u64, Ordering::Relaxed);
    tracing::warn!(
        overflow = overflow,
        cap = MAX_BUFFER_RECORDS,
        "plugin_stats retry buffer at capacity; oldest records evicted"
    );
}

/// Attempt to insert the buffered batch. On success the buffer is cleared
/// and the consecutive-failure counter is reset. On error the buffer is
/// RETAINED for the next tick. After `MAX_CONSECUTIVE_FLUSH_FAILURES`
/// consecutive failures the buffer is cleared as a last-resort safety vent
/// and the eviction counter records the loss.
fn flush(
    store: &SqliteStore,
    buf: &mut Vec<PluginStatRecord>,
    failed_flushes: &AtomicU64,
    evicted: &AtomicU64,
    consecutive_failures: &mut u32,
) {
    if buf.is_empty() {
        return;
    }
    match store.insert_plugin_stats_batch(buf) {
        Ok(()) => {
            buf.clear();
            *consecutive_failures = 0;
        }
        Err(e) => {
            failed_flushes.fetch_add(1, Ordering::Relaxed);
            *consecutive_failures = consecutive_failures.saturating_add(1);
            tracing::warn!(
                error = %e,
                attempt = *consecutive_failures,
                retained = buf.len(),
                "failed to flush plugin_stats batch; retaining for retry"
            );
            if *consecutive_failures >= MAX_CONSECUTIVE_FLUSH_FAILURES {
                evicted.fetch_add(buf.len() as u64, Ordering::Relaxed);
                tracing::error!(
                    attempts = *consecutive_failures,
                    evicted = buf.len(),
                    "plugin_stats writer giving up after consecutive failures; dropping buffer"
                );
                buf.clear();
                *consecutive_failures = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use voom_domain::plugin_stats::PluginInvocationOutcome;

    fn rec(i: u64) -> PluginStatRecord {
        PluginStatRecord {
            plugin_id: "x".into(),
            event_type: "y".into(),
            started_at: Utc::now(),
            duration_ms: i,
            outcome: PluginInvocationOutcome::Ok,
        }
    }

    #[test]
    fn records_flush_to_store_within_one_second() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        {
            let sink = SqliteStatsSink::new(store.clone());
            for i in 0..50 {
                sink.record(rec(i));
            }
            // Wait for at least one periodic flush to fire.
            std::thread::sleep(Duration::from_millis(800));
            let conn = store.conn().unwrap();
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
                .unwrap();
            assert!(
                count >= 50,
                "expected at least 50 rows after periodic flush, got {count}"
            );
            // sink dropped here — additional cleanup not relied on
        }
    }

    #[test]
    fn drop_flushes_remaining_records() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        {
            let sink = SqliteStatsSink::new(store.clone());
            for i in 0..10 {
                sink.record(rec(i));
            }
            // drop sink → writer thread joins after final flush
        }
        let conn = store.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 10);
    }

    #[test]
    fn channel_overflow_does_not_panic() {
        // Smoke test: spamming record() at saturating speed must never panic,
        // even when the bounded channel overflows. We cannot deterministically
        // assert `dropped_count > 0` here because the writer-thread drain rate
        // varies with hardware. See `overflow_increments_dropped_counter_when_channel_full`
        // for deterministic overflow coverage.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let sink = SqliteStatsSink::new(store);
        for i in 0..50_000u64 {
            sink.record(rec(i));
        }
        std::thread::sleep(Duration::from_millis(200));
        // No assertion beyond no-panic: dropped_count() is informational only.
        let _ = sink.dropped_count();
    }

    #[test]
    fn overflow_increments_dropped_counter_when_channel_full() {
        // capacity=0 makes the channel "rendezvous": try_send succeeds only when
        // a receiver is actively recv'ing. The writer thread is parked in
        // recv_timeout for up to 500ms between iterations, so the overwhelming
        // majority of these try_send calls will see a full channel and drop.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let sink = SqliteStatsSink::with_capacity_for_tests(store, 0);
        for i in 0..200u64 {
            sink.record(rec(i));
        }
        // Allow scheduler slack: with capacity=0 and a single writer parked in
        // recv_timeout, ≥150 of 200 try_sends must fail. Concrete number tuned
        // to be robust against scheduler noise without becoming vacuous.
        assert!(
            sink.dropped_count() >= 150,
            "expected at least 150 drops with rendezvous channel, got {}",
            sink.dropped_count()
        );
    }

    #[test]
    fn flush_failure_retains_buffer_and_recovers() {
        // Drive the writer's flush() directly so we can simulate a store
        // that fails the first attempt, then succeeds. Bypasses the channel.
        use std::sync::Arc;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // Drop the underlying table to force the next insert to fail, then
        // recreate it before the second attempt. This exercises the retain-
        // on-error path without requiring a mock storage implementation.
        {
            let conn = store.conn().unwrap();
            conn.execute("DROP TABLE plugin_stats", []).unwrap();
        }

        let failed_flushes = Arc::new(AtomicU64::new(0));
        let evicted = Arc::new(AtomicU64::new(0));
        let mut buf = vec![rec(1), rec(2), rec(3)];
        let mut consecutive = 0u32;

        super::flush(
            &store,
            &mut buf,
            &failed_flushes,
            &evicted,
            &mut consecutive,
        );
        assert_eq!(failed_flushes.load(Ordering::Relaxed), 1);
        assert_eq!(buf.len(), 3, "buffer must be retained after failure");
        assert_eq!(evicted.load(Ordering::Relaxed), 0);
        assert_eq!(consecutive, 1);

        // Now recreate the table and re-run flush; the same records should land.
        {
            let conn = store.conn().unwrap();
            crate::schema::create_schema(&conn).unwrap();
        }
        super::flush(
            &store,
            &mut buf,
            &failed_flushes,
            &evicted,
            &mut consecutive,
        );
        assert_eq!(buf.len(), 0, "buffer must be cleared after success");
        assert_eq!(consecutive, 0);

        let conn = store.conn().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM plugin_stats", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn shutdown_evicts_records_when_final_flush_fails() {
        // The Disconnected arm must not silently drop the buffer on shutdown.
        // If the final flush cannot succeed within the shutdown deadline, the
        // surviving buffer must be accounted for in `evicted` and surfaced via
        // tracing — never lost without trace. (Codex adversarial review, May 2026)
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let store = Arc::new(SqliteStore::in_memory().unwrap());
        // Persistently fail every insert by dropping the table.
        {
            let conn = store.conn().unwrap();
            conn.execute("DROP TABLE plugin_stats", []).unwrap();
        }

        // The sink owns evicted: Arc<AtomicU64>. We need to observe it AFTER
        // the sink is dropped, so capture a clone of the Arc via a test-only
        // accessor before drop.
        let sink = SqliteStatsSink::new(store.clone());
        let evicted_handle: Arc<AtomicU64> = sink.evicted_handle_for_tests();

        for i in 0..10 {
            sink.record(rec(i));
        }
        // Allow the writer thread to receive and try (and fail) at least one
        // flush before shutdown.
        std::thread::sleep(Duration::from_millis(600));

        drop(sink);

        let lost = evicted_handle.load(Ordering::Relaxed);
        assert!(
            lost >= 10,
            "expected >= 10 evicted records after persistent flush failure on shutdown, got {lost}"
        );
    }

    #[test]
    fn flush_gives_up_after_consecutive_failures_and_evicts() {
        // Persistently failing store: drop the table and never recreate.
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        {
            let conn = store.conn().unwrap();
            conn.execute("DROP TABLE plugin_stats", []).unwrap();
        }

        let failed_flushes = Arc::new(AtomicU64::new(0));
        let evicted = Arc::new(AtomicU64::new(0));
        let mut buf = vec![rec(1), rec(2)];
        let mut consecutive = 0u32;

        for _ in 0..super::MAX_CONSECUTIVE_FLUSH_FAILURES {
            super::flush(
                &store,
                &mut buf,
                &failed_flushes,
                &evicted,
                &mut consecutive,
            );
        }
        assert_eq!(buf.len(), 0, "buffer must be evicted after cap reached");
        assert_eq!(evicted.load(Ordering::Relaxed), 2);
        assert_eq!(consecutive, 0, "counter resets after eviction");
    }
}
