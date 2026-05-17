# voom-plugin-sdk

SDK for authoring VOOM plugins — both native (Rust) and WASM (any
language with a WIT-capable toolchain). Covers the rev-6 plugin contract
(#378).

## Implementing `on_call`

`Plugin::on_call` handles unary and streaming RPCs the kernel routes to
your plugin via `dispatch_to_capability`. The trait takes `&Call` (the
kernel keeps ownership for stats accounting) and returns
`voom_domain::errors::Result<CallResponse>`. The `Call` enum is
`#[non_exhaustive]`; match the variant you handle with a `let ... else`
guard and reject the rest with an explicit error:

```rust
use voom_domain::call::{Call, CallResponse};
use voom_domain::errors::{Result, VoomError};

impl Plugin for MyEvaluator {
    fn on_call(&self, call: &Call) -> Result<CallResponse> {
        let Call::EvaluatePolicy { policy, file, .. } = call else {
            return Err(VoomError::plugin(
                self.name(),
                format!(
                    "{} only handles Call::EvaluatePolicy, got {:?}",
                    self.name(),
                    std::mem::discriminant(call)
                ),
            ));
        };
        // `policy` and `file` are `&Box<...>` — deref coercion lets them
        // flow into helpers that take `&CompiledPolicy` / `&MediaFile`.
        let result = self.evaluate(policy, file)?;
        Ok(CallResponse::EvaluatePolicy(result))
    }
}
```

## Claiming a capability

Return one or more `Capability` values from `Plugin::capabilities()`.
The trait returns `&[Capability]`, so plugins typically store the claim
list in a field set up by `new()`. Each capability has a resolution
discipline (`Exclusive`, `Sharded`, or `Shared`); claiming an Exclusive
capability that another plugin already claims is a registration error.

```rust
pub struct MyEvaluator {
    capabilities: Vec<Capability>,
}

impl MyEvaluator {
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::EvaluatePolicy],   // Exclusive
        }
    }
}

impl Plugin for MyEvaluator {
    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}
```

The kernel routes `Call::EvaluatePolicy` to the single plugin claiming
`Capability::EvaluatePolicy`. To take over the capability in a custom
build, disable the default plugin via `disabled_plugins` in `config.toml`
and register your own.

## Streaming Calls

A streaming Call is a `Call` variant whose payload contains a
`tokio::sync::mpsc::Sender<T>`. The plugin sends items through the sender
while running; the host consumes from the matching receiver. Backpressure
is the channel's natural backpressure — a saturated consumer blocks the
plugin's `blocking_send`.

Because `on_call` receives `&Call`, the destructured fields are
references: `sink: &Sender<...>`, `cancel: &CancellationToken`, and
`root_done: &Option<Sender<...>>`. Calling `sink.blocking_send(...)` and
`cancel.is_cancelled()` works directly through the references.

```rust
let Call::ScanLibrary {
    uri,
    options,
    sink,           // &mpsc::Sender<FileDiscoveredEvent>
    root_done,      // &Option<mpsc::Sender<RootWalkCompletedEvent>>
    cancel,         // &CancellationToken — observe between sends
    ..
} = call else {
    return Err(VoomError::plugin(self.name(), "expected ScanLibrary"));
};
for entry in walk(uri, options) {
    if cancel.is_cancelled() { break; }
    sink.blocking_send(make_event(entry))
        .map_err(|e| VoomError::plugin(self.name(), e.to_string()))?;
}
// `root_done` is optional; emit a completion event through it if the
// host wired one up (e.g. so the per-root execution gate can open).
if let Some(_rd) = root_done {
    // build the appropriate RootWalkCompletedEvent for your scan and
    // send it through `_rd.blocking_send(...)`.
}
Ok(CallResponse::ScanLibrary(summary))
```

WASM plugins use the host import `emit-call-item` (defined in
`crates/voom-wit/wit/host.wit`) to push items; cancellation is observed
via `call-is-cancelled`.

## See also

- `docs/architecture.md` — Communication primitives & Capability-based
  routing.
- `docs/superpowers/specs/2026-05-14-plugin-stats-self-reporting-design.md`
  — design rationale.
