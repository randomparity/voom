//! Criterion benchmark for `EventBus::publish` stats overhead.
//!
//! Issue #379 — measures the dispatcher's per-event cost across three
//! configurations:
//!
//! 1. `noop_sink_fanout5` — bus with `NoopStatsSink` (the production
//!    default when sqlite-store is disabled). Establishes the baseline.
//! 2. `vec_sink_fanout5` — bus with a `Mutex<Vec<PluginStatRecord>>` sink.
//!    Measures the per-handler instrumentation cost (one record
//!    allocation + one virtual-call into the sink + one mutex lock).
//! 3. `sqlite_sink` coverage is provided by
//!    `plugins/sqlite-store/benches/stats_sink.rs` instead — adding it
//!    here would require an inverted dependency on `voom-sqlite-store`.
//!
//! Workload: dispatch 5-way fan-out of `Event::FileDiscovered` to five
//! no-op subscribers per iteration. Numbers should be compared as
//! medians; the issue's <5% regression threshold is informational
//! (no CI gate added).

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use parking_lot::Mutex;
use voom_domain::Capability;
use voom_domain::errors::Result as DomainResult;
use voom_domain::events::{Event, EventResult, FileDiscoveredEvent};
use voom_domain::plugin_stats::PluginStatRecord;
use voom_kernel::Plugin;
use voom_kernel::bus::EventBus;
use voom_kernel::stats_sink::{NoopStatsSink, StatsSink};

/// No-op subscriber that handles every event by returning `Ok(None)`
/// (counts as `Skipped` in the dispatcher's outcome classifier).
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

/// Sink that pushes each record into an unbounded Vec. Measures the
/// per-handler instrumentation cost (allocate record + one virtual call
/// + one mutex acquisition).
struct VecSink {
    inner: Mutex<Vec<PluginStatRecord>>,
}

impl StatsSink for VecSink {
    fn record(&self, record: PluginStatRecord) {
        self.inner.lock().push(record);
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

fn dispatch_one(bus: &EventBus) {
    let ev = Event::FileDiscovered(FileDiscoveredEvent::new(
        PathBuf::from("/x"),
        0,
        None,
    ));
    let _ = bus.publish(ev);
}

fn bench_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch");
    group.sample_size(50);

    // 1. Noop sink — baseline.
    {
        let bus = build_bus(Arc::new(NoopStatsSink), 5);
        group.bench_function("noop_sink_fanout5", |b| {
            b.iter(|| dispatch_one(&bus));
        });
    }

    // 2. Vec sink — per-handler instrumentation cost.
    {
        let sink = Arc::new(VecSink {
            inner: Mutex::new(Vec::new()),
        });
        let bus = build_bus(sink, 5);
        group.bench_function("vec_sink_fanout5", |b| {
            b.iter(|| dispatch_one(&bus));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
