//! Criterion benchmark for `EventBus::publish` stats overhead.
//!
//! Issue #379 — measures the dispatcher's per-event cost across two
//! configurations:
//!
//! 1. `noop_sink_10k_fanout5` — bus with `NoopStatsSink` (the production
//!    default when sqlite-store is disabled). Establishes the baseline.
//! 2. `vec_sink_10k_fanout5` — bus with a `Mutex<Vec<PluginStatRecord>>`
//!    sink. Measures the per-handler instrumentation cost (one record
//!    allocation + one virtual call + one mutex acquisition per
//!    subscriber).
//!
//! Workload (per Criterion sample): dispatch 10,000 `FileDiscovered`
//! events to a 5-way fan-out of no-op subscribers. The Vec sink is
//! pre-allocated to the expected record count via `iter_batched` so the
//! measurement does not include reallocation cost or retained heap
//! pressure across samples.
//!
//! The SQLite sink variant lives in
//! `plugins/sqlite-store/benches/stats_sink.rs` — adding it here would
//! invert the kernel → sqlite-store dependency.

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use parking_lot::Mutex;
use voom_domain::Capability;
use voom_domain::errors::Result as DomainResult;
use voom_domain::events::{Event, EventResult, FileDiscoveredEvent};
use voom_domain::plugin_stats::PluginStatRecord;
use voom_kernel::Plugin;
use voom_kernel::bus::EventBus;
use voom_kernel::stats_sink::{NoopStatsSink, StatsSink};

const EVENTS_PER_SAMPLE: usize = 10_000;
const FANOUT: usize = 5;

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

/// Sink that pushes each record into a `Mutex<Vec>`. The Vec is
/// pre-allocated to expected capacity per sample to remove allocation
/// jitter from the measurement.
struct VecSink {
    inner: Mutex<Vec<PluginStatRecord>>,
}

impl StatsSink for VecSink {
    fn record(&self, record: PluginStatRecord) {
        self.inner.lock().push(record);
    }
}

fn build_bus(sink: Arc<dyn StatsSink>) -> EventBus {
    let bus = EventBus::with_stats_sink(sink);
    for i in 0..FANOUT {
        bus.subscribe_plugin(
            Arc::new(NoopSubscriber {
                name: format!("noop-{i}"),
            }),
            0,
        );
    }
    bus
}

fn dispatch_batch(bus: &EventBus) {
    for _ in 0..EVENTS_PER_SAMPLE {
        let ev = Event::FileDiscovered(FileDiscoveredEvent::new(PathBuf::from("/x"), 0, None));
        let _ = bus.publish(ev);
    }
}

fn bench_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch");
    // Each sample is a 10k-event batch; ten samples is enough for a
    // stable median estimate without dragging the bench out.
    group.sample_size(10);

    // 1. Noop sink — baseline. The sink is stateless so we can reuse
    //    the bus across samples.
    {
        let bus = build_bus(Arc::new(NoopStatsSink));
        group.bench_function("noop_sink_10k_fanout5", |b| {
            b.iter(|| dispatch_batch(&bus));
        });
    }

    // 2. Vec sink — per-handler instrumentation cost. `iter_batched`
    //    with `LargeInput` reconstructs the bus AND the sink between
    //    samples so the previous sample's Vec contents are dropped
    //    before the next sample begins. The Vec is pre-allocated to
    //    its final size so we don't measure reallocation.
    group.bench_function("vec_sink_10k_fanout5", |b| {
        b.iter_batched(
            || {
                let cap = EVENTS_PER_SAMPLE * FANOUT;
                let sink: Arc<dyn StatsSink> = Arc::new(VecSink {
                    inner: Mutex::new(Vec::with_capacity(cap)),
                });
                build_bus(sink)
            },
            |bus| {
                dispatch_batch(&bus);
            },
            BatchSize::LargeInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
