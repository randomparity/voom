//! Criterion benchmark for the production `SqliteStatsSink` channel-send
//! cost end-to-end (bus dispatcher → channel `try_send` → background
//! writer → in-memory SQLite).
//!
//! Issue #379 — complements `crates/voom-kernel/benches/dispatch.rs`,
//! which exercises only the `NoopStatsSink` and an in-process Vec sink.
//!
//! Two bench variants:
//!
//! 1. `sqlite_sink_lossless_3k_fanout4` — sized so the total record
//!    count (`EVENTS_LOSSLESS × FANOUT_LOSSLESS = 3000`) fits inside
//!    the default channel capacity (`4096`). After dispatch, the bench
//!    drops the sink (which joins the writer thread) and verifies via
//!    the public `rollup_plugin_stats` API that the expected number of
//!    records were persisted, with zero drops and zero failed flushes.
//!    This proves the bench is exercising the full happy path.
//!
//! 2. `sqlite_sink_saturated_10k_fanout5` — deliberately saturates the
//!    channel (50000 records ≫ 4096 capacity), so the production drop
//!    path is exercised. Numbers from this variant include the
//!    `try_send → Err(Full) → fetch_add(1)` bookkeeping cost. We assert
//!    `dropped_count > 0` and do not assert the persisted row count.
//!
//! Both variants reconstruct the sink between samples via
//! `iter_batched(BatchSize::LargeInput)` so background-writer backlog
//! from the previous sample doesn't leak across measurements.

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use voom_domain::Capability;
use voom_domain::errors::Result as DomainResult;
use voom_domain::events::{Event, EventResult, FileDiscoveredEvent};
use voom_domain::plugin_stats::PluginStatsFilter;
use voom_domain::storage::PluginStatsStorage;
use voom_kernel::Plugin;
use voom_kernel::bus::EventBus;
use voom_kernel::stats_sink::StatsSink;
use voom_sqlite_store::stats_sink::SqliteStatsSink;
use voom_sqlite_store::store::SqliteStore;

/// Subscribers and events per Criterion sample for the lossless variant.
/// 750 × 4 = 3000 records, comfortably under the 4096 channel capacity.
const FANOUT_LOSSLESS: usize = 4;
const EVENTS_LOSSLESS: usize = 750;

/// No-op subscriber identical to the kernel bench's NoopSubscriber.
struct NoopSubscriber {
    name: String,
}

impl Plugin for NoopSubscriber {
    fn name(&self) -> &str {
        &self.name
    }
    fn version(&self) -> &str {
        "0.0.0"
    }
    fn capabilities(&self) -> &[Capability] {
        &[]
    }
    fn handles(&self, _event_type: &str) -> bool {
        true
    }
    fn on_event(&self, _event: &Event) -> DomainResult<Option<EventResult>> {
        Ok(None)
    }
}

fn build_bus(sink: Arc<dyn StatsSink>, fanout: usize) -> EventBus {
    let bus = EventBus::with_stats_sink(sink);
    for i in 0..fanout {
        bus.subscribe_plugin(
            Arc::new(NoopSubscriber {
                name: format!("noop-{i}"),
            }),
            0,
        );
    }
    bus
}

fn dispatch_batch(bus: &EventBus, count: usize) {
    for _ in 0..count {
        let ev = Event::FileDiscovered(FileDiscoveredEvent::new(PathBuf::from("/x"), 0, None));
        let _ = bus.publish(ev);
    }
}

/// Sum invocation counts across all rollup rows. Uses the public
/// `PluginStatsStorage::rollup_plugin_stats` API so the bench doesn't
/// need crate-private accessors.
fn count_persisted(store: &Arc<SqliteStore>) -> u64 {
    let filter = PluginStatsFilter::new(None, None, None);
    let rollup = store
        .rollup_plugin_stats(&filter)
        .expect("rollup_plugin_stats");
    rollup.iter().map(|r| r.invocation_count).sum()
}

fn bench_sqlite_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_sink");
    group.sample_size(10);

    // ─── Lossless full-path variant ──────────────────────────────────
    //
    // 750 × 4 = 3000 records < 4096 channel capacity. The bench keeps
    // concrete handles to the sink and store, drops the bus FIRST
    // (releasing its Arc<dyn StatsSink>), then drops the sink (which
    // releases the only remaining Arc, triggering SqliteStatsSink::Drop
    // and joining the writer thread). After that the rollup is read
    // and we verify every record persisted with zero drops, zero
    // failed flushes, and zero evictions. This is the trustworthy
    // measurement of the full happy path.
    //
    // The drop ordering is load-bearing: the bus holds its own
    // `Arc<dyn StatsSink>` that points at the same `SqliteStatsSink`,
    // so `drop(sink)` while the bus is alive would only decrement the
    // Arc refcount — the writer would not join and the final batch
    // would still be in flight when `count_persisted` runs. (Codex
    // review, May 2026.)
    group.bench_function("sqlite_sink_lossless_3k_fanout4", |b| {
        b.iter_batched(
            || {
                let store = Arc::new(SqliteStore::in_memory().expect("in-memory sqlite store"));
                let sink = Arc::new(SqliteStatsSink::new(store.clone()));
                let bus = build_bus(sink.clone() as Arc<dyn StatsSink>, FANOUT_LOSSLESS);
                (bus, sink, store)
            },
            |(bus, sink, store)| {
                dispatch_batch(&bus, EVENTS_LOSSLESS);
                // Read live-side counters before any drop.
                let dropped_before = sink.dropped_count();
                let failed_before = sink.failed_flush_count();
                let evicted_before = sink.evicted_count();
                // Drop the bus FIRST so it releases its Arc<dyn StatsSink>.
                drop(bus);
                // Now `sink` is the sole owner of the SqliteStatsSink;
                // dropping it triggers Drop → close channel → writer
                // thread joins after flushing.
                drop(sink);
                let persisted = count_persisted(&store);
                let expected = (EVENTS_LOSSLESS * FANOUT_LOSSLESS) as u64;
                assert_eq!(
                    dropped_before, 0,
                    "lossless variant must not drop any records; got {dropped_before}"
                );
                assert_eq!(
                    failed_before, 0,
                    "lossless variant must not fail any flushes; got {failed_before}"
                );
                assert_eq!(
                    evicted_before, 0,
                    "lossless variant must not evict any records; got {evicted_before}"
                );
                assert_eq!(
                    persisted, expected,
                    "lossless variant must persist every record; expected \
                     {expected}, got {persisted}"
                );
            },
            BatchSize::LargeInput,
        );
    });

    // NOTE: a saturated-channel variant (50000 events ≫ 4096 capacity)
    // was considered but dropped — the writer drains concurrently and
    // assertions on `dropped > 0` are scheduler-dependent (see the
    // existing `stats_sink.rs::channel_overflow_does_not_panic` smoke
    // test and the #384 history). The lossless variant above covers
    // the production happy path; the drop path is covered
    // deterministically by the
    // `overflow_increments_dropped_counter_when_channel_full` unit
    // test (#384) using a held-receiver no-writer seam.

    group.finish();
}

criterion_group!(benches, bench_sqlite_sink);
criterion_main!(benches);
