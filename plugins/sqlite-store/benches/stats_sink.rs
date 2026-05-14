//! Criterion benchmark for the production `SqliteStatsSink` channel-send
//! cost end-to-end (bus dispatcher → channel `try_send` → background
//! writer → in-memory SQLite).
//!
//! Issue #379 — complements `crates/voom-kernel/benches/dispatch.rs`,
//! which exercises only the `NoopStatsSink` and an in-process Vec sink.
//! This bench measures what production VOOM actually pays per dispatch
//! when stats are persisted.

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use voom_domain::Capability;
use voom_domain::errors::Result as DomainResult;
use voom_domain::events::{Event, EventResult, FileDiscoveredEvent};
use voom_kernel::Plugin;
use voom_kernel::bus::EventBus;
use voom_kernel::stats_sink::StatsSink;
use voom_sqlite_store::stats_sink::SqliteStatsSink;
use voom_sqlite_store::store::SqliteStore;

const EVENTS_PER_SAMPLE: usize = 10_000;
const FANOUT: usize = 5;

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
        let ev = Event::FileDiscovered(FileDiscoveredEvent::new(
            PathBuf::from("/x"),
            0,
            None,
        ));
        let _ = bus.publish(ev);
    }
}

fn bench_sqlite_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("stats_sink");
    group.sample_size(10);

    // Reconstruct the sink AND the bus per sample so background-writer
    // backlog from the previous sample doesn't leak in.
    group.bench_function("sqlite_sink_10k_fanout5", |b| {
        b.iter_batched(
            || {
                let store = Arc::new(
                    SqliteStore::in_memory().expect("in-memory sqlite store"),
                );
                let sink: Arc<dyn StatsSink> = Arc::new(SqliteStatsSink::new(store));
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

criterion_group!(benches, bench_sqlite_sink);
criterion_main!(benches);
