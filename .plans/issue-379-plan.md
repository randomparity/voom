# Plan: Criterion benchmark for EventBus::publish stats overhead (#379)

## Problem

#92 added per-dispatch instrumentation in `EventBus::publish_recursive`.
The acceptance criterion called for <5% regression at 1000-file scan;
the benchmark was deferred. We need a criterion benchmark that
characterizes the overhead of:

1. Baseline: `EventBus` with `NoopStatsSink`.
2. In-memory recording sink (push to `Mutex<Vec<...>>` — measures
   per-handler instrumentation cost).
3. `SqliteStatsSink` backed by `:memory:` (measures channel-send cost).

## Workload

Per the issue: dispatch 10k events to a fan-out of 5 subscribers, each
a no-op handler.

## Changes

### `crates/voom-kernel/Cargo.toml`

Add a dev-dependency:

```toml
[dev-dependencies]
criterion = "0.5"

[[bench]]
name = "dispatch"
harness = false
```

(Reuse the same `criterion` version `voom-sqlite-store` already depends
on — `=0.8.2`. Don't introduce a new version.)

### `crates/voom-kernel/benches/dispatch.rs` (NEW)

```rust
use std::sync::Arc;
use criterion::{Criterion, criterion_group, criterion_main};
use parking_lot::Mutex;
use voom_domain::events::Event;
use voom_domain::plugin_stats::PluginStatRecord;
use voom_kernel::Plugin;
use voom_kernel::stats_sink::StatsSink;
use voom_kernel::bus::EventBus;

struct NoopPlugin {
    name: String,
}

impl Plugin for NoopPlugin {
    fn name(&self) -> &str { &self.name }
    fn handles(&self, _: &str) -> bool { true }
    fn on_event(&self, _ev: &Event) -> voom_domain::errors::Result<Option<voom_domain::events::EventResult>> {
        Ok(None) // counts as Skipped
    }
}

struct VecSink {
    inner: Mutex<Vec<PluginStatRecord>>,
}

impl StatsSink for VecSink {
    fn record(&self, r: PluginStatRecord) {
        self.inner.lock().push(r);
    }
}

fn build_bus(sink: Arc<dyn StatsSink>, fanout: usize) -> EventBus {
    let bus = EventBus::with_stats_sink(sink);
    for i in 0..fanout {
        bus.subscribe_plugin(Arc::new(NoopPlugin { name: format!("p{i}") }), 0);
    }
    bus
}

fn dispatch_event(bus: &EventBus) {
    // Cheap event to dispatch — FileDiscovered with empty path.
    let ev = Event::FileDiscovered(voom_domain::events::FileDiscoveredEvent::new(
        std::path::PathBuf::from("/x"),
        0,
        None,
    ));
    bus.publish(ev);
}

fn bench_dispatch(c: &mut Criterion) {
    let mut group = c.benchmark_group("dispatch");
    group.sample_size(50);

    {
        let sink = Arc::new(voom_kernel::stats_sink::NoopStatsSink);
        let bus = build_bus(sink, 5);
        group.bench_function("noop_sink_fanout5", |b| {
            b.iter(|| dispatch_event(&bus));
        });
    }

    {
        let sink = Arc::new(VecSink { inner: Mutex::new(Vec::new()) });
        let bus = build_bus(sink, 5);
        group.bench_function("vec_sink_fanout5", |b| {
            b.iter(|| dispatch_event(&bus));
        });
    }

    // The SqliteStatsSink test requires the sqlite-store crate; do it
    // separately if needed. Cross-crate benches are out of scope.

    group.finish();
}

criterion_group!(benches, bench_dispatch);
criterion_main!(benches);
```

Note: testing the `SqliteStatsSink` variant requires the sqlite-store
crate as a dev-dep, which would invert the crate dependency graph
(kernel → sqlite-store). Two reasonable options:

- Add the bench in `plugins/sqlite-store/benches/stats_sink.rs` instead,
  measuring the bus dispatch + sink combo end-to-end.
- Do baseline + vec only in kernel; defer sqlite variant to a separate
  bench file in plugins/sqlite-store.

Plan picks **the latter**: kernel bench covers Noop+Vec; sqlite-store
bench covers the SQLite case. This keeps each crate's bench self-
contained.

### `plugins/sqlite-store/benches/stats_sink.rs` (NEW)

Similar structure: build an `EventBus::with_stats_sink(SqliteStatsSink)`
and dispatch 10k events. Reuses `NoopPlugin` (copied or split into a
shared `voom-kernel::testing` helper).

### `docs/architecture.md`

Add a brief note in the "Bus dispatcher instrumentation" subsection
referencing the bench: "Measured overhead per handler dispatch: ~X ns
baseline, ~Y ns with vec sink, ~Z ns with SqliteStatsSink (see
`crates/voom-kernel/benches/dispatch.rs` and
`plugins/sqlite-store/benches/stats_sink.rs`)." Fill in numbers from
the actual run.

## CI gating

The issue asks "CI gates against a >5% median regression if we want to
be strict (optional)." Skipping CI gating for now — adding a regression
gate to CI requires:

- Storing baseline timings.
- Either criterion-compare-flame (third-party) or hand-rolled diff.
- Tolerance tuning for runner variability.

Out of scope; leave as a follow-up note in the bench file.

## Test plan

- `cargo bench -p voom-kernel dispatch` runs and produces output.
- `cargo bench -p voom-sqlite-store stats_sink` runs and produces output.
- Both can also be invoked under `cargo test --benches` to verify they
  compile.

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo bench -p voom-kernel dispatch -- --warm-up-time=1 --measurement-time=2
cargo bench -p voom-sqlite-store stats_sink -- --warm-up-time=1 --measurement-time=2
```

## Out of scope

- Adding a CI regression gate.
- Comparing against a stored baseline file.
- Benchmarking other parts of the bus (cascade, recursion).
