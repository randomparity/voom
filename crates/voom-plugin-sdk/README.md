# voom-plugin-sdk

SDK for authoring VOOM plugins — both native (Rust) and WASM (any
language with a WIT-capable toolchain). Covers the rev-6 plugin contract
(#378).

## Implementing `on_call`

`Plugin::on_call` handles unary and streaming RPCs the kernel routes to
your plugin via `dispatch_to_capability`. The `Call` enum is
`#[non_exhaustive]`; match the variants you handle and reject the rest
with an explicit error:

```rust
use voom_domain::call::{Call, CallResponse};

impl Plugin for MyEvaluator {
    fn on_call(&self, call: Call) -> anyhow::Result<CallResponse> {
        match call {
            Call::EvaluatePolicy { policy, file, .. } => {
                let result = self.evaluate(*policy, *file)?;
                Ok(CallResponse::EvaluatePolicy(result))
            }
            other => anyhow::bail!(
                "{} does not handle {:?}",
                self.name(),
                other
            ),
        }
    }
}
```

## Claiming a capability

Return one or more `Capability` values from `Plugin::capabilities()`.
Each capability has a resolution discipline (`Exclusive`, `Sharded`, or
`Shared`); claiming an Exclusive capability that another plugin already
claims is a registration error.

```rust
impl Plugin for MyEvaluator {
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::EvaluatePolicy]   // Exclusive
    }
}
```

The kernel routes `Call::EvaluatePolicy` to the single plugin claiming
`Capability::EvaluatePolicy`. To take over the capability in a custom
build, disable the default plugin via `disabled_plugins` in `config.toml`
and register your own.

## Streaming Calls

A streaming Call is a `Call` variant whose payload contains an
`mpsc::Sender<T>`. The plugin sends items through the sender while
running; the host consumes from the matching receiver. Backpressure is
the channel's natural backpressure — a saturated consumer blocks the
plugin's `blocking_send`.

```rust
Call::ScanLibrary {
    uri,
    options,
    scan_session,
    sink,           // mpsc::Sender<FileDiscoveredEvent>
    root_done,
    cancel,         // CancellationToken — observe between sends
} => {
    for entry in walk(&uri, &options) {
        if cancel.is_cancelled() { break; }
        sink.blocking_send(make_event(entry))?;
    }
    Ok(CallResponse::ScanLibrary(summary))
}
```

WASM plugins use the host import `emit-call-item` (defined in
`crates/voom-wit/wit/host.wit`) to push items; cancellation is observed
via `call-is-cancelled`.

## See also

- `docs/architecture.md` — Communication primitives & Capability-based
  routing.
- `docs/superpowers/specs/2026-05-14-plugin-stats-self-reporting-design.md`
  — design rationale.
