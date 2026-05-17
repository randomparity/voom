# voom-plugin-sdk

Helpers for authoring VOOM **WASM** plugins. This crate intentionally
does not depend on `voom-kernel` — it ships only the types, ABI helpers,
and host shims a `wasm32-wasip1` guest needs.

VOOM also supports **native** plugins (Rust crates compiled into the
binary that implement `voom_kernel::Plugin` directly). Native plugins
do not use this SDK; they depend on `voom-kernel` and `voom-domain`
instead. See the "Native plugins" section below for the contract surface
and `docs/plugin-development.md` for the full walkthrough.

Both tiers participate in the same rev-6 plugin contract (#378):
capability claims, unary and streaming Calls routed via
`Kernel::dispatch_to_capability`, and instrumented `plugin_stats`
records.

## Native plugins (compiled into the binary)

`Cargo.toml` dependencies: `voom-kernel`, `voom-domain`. Not
`voom-plugin-sdk`.

Plugins implement `voom_kernel::Plugin` and override `on_call` to handle
unary or streaming Calls the kernel routes via
`Kernel::dispatch_to_capability`:

```rust
use voom_domain::call::{Call, CallResponse};
use voom_domain::capabilities::Capability;
use voom_domain::errors::{Result, VoomError};
use voom_kernel::Plugin;

pub struct MyEvaluator {
    capabilities: Vec<Capability>,
}

impl MyEvaluator {
    pub fn new() -> Self {
        Self {
            capabilities: vec![Capability::EvaluatePolicy], // Exclusive
        }
    }
}

impl Plugin for MyEvaluator {
    fn name(&self) -> &str { "my-evaluator" }
    fn version(&self) -> &str { "0.1.0" }
    fn capabilities(&self) -> &[Capability] { &self.capabilities }

    fn on_call(&self, call: &Call) -> Result<CallResponse> {
        let Call::EvaluatePolicy { policy, file, .. } = call else {
            return Err(VoomError::plugin(
                self.name(),
                format!(
                    "{} only handles Call::EvaluatePolicy, got {:?}",
                    self.name(),
                    std::mem::discriminant(call),
                ),
            ));
        };
        // `policy` and `file` are `&Box<...>`; deref coercion lets them
        // flow into helpers that take `&CompiledPolicy` / `&MediaFile`.
        let result = self.evaluate(policy, file)?;
        Ok(CallResponse::EvaluatePolicy(result))
    }
}
```

### Streaming Calls (native)

Streaming-Call variants of `Call` (e.g. `Call::ScanLibrary`) carry a
`tokio::sync::mpsc::Sender<T>` plus a `CancellationToken`. Because
`on_call` takes `&Call`, the destructured fields are references; the
plugin emits items through `sink.blocking_send(...)` and observes
cancellation via `cancel.is_cancelled()`. See `docs/architecture.md`
("Communication primitives — Events vs Calls") for a full example.

## WASM plugins

`Cargo.toml` dependencies: `voom-plugin-sdk`, `wit-bindgen`. WIT files
live in `crates/voom-wit/wit/`.

WASM plugins do not implement `voom_kernel::Plugin`. They expose three
free functions — `get_info`, `handles`, `on_event`, and (for plugins
that claim a Call-handling capability) an `on_call` — and the
SDK-driven `wit_bindgen::generate!` + `export!` wiring connects them to
the WIT `Guest` exports. The shape that matches the in-tree
`wasm-plugins/example-metadata` and `wasm-plugins/radarr-metadata`
templates:

```rust
use voom_plugin_sdk::{
    Capability, Event, HostFunctions, OnEventResult, PluginInfoData,
    WasmCall, CallResponse, decode_call, encode_response,
};

// Identity + capability claim. Capabilities are the same
// `voom_domain::capabilities::Capability` enum the native side uses,
// re-exported from voom-plugin-sdk.
pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "my-plugin",
        "0.1.0",
        vec![Capability::EnrichMetadata { source: "my-plugin".into() }],
    )
    .with_description("Example metadata enrichment plugin")
    .with_license("MIT")
}

pub fn handles(event_type: &str) -> bool {
    event_type == Event::FILE_INTROSPECTED
}

pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    // ... see wasm-plugins/example-metadata for a worked example.
    let _ = (event_type, payload, host);
    None
}
```

### Handling Calls in WASM

For plugins that claim a Call-handling capability, decode the host-supplied
bytes into the WASM-safe mirror `WasmCall`, do the work, then encode the
`CallResponse`. `WasmCall` omits the non-serde fields (`sink`, `root_done`,
`cancel`) — those are host-only; streaming emission goes through host
imports (see below).

```rust
pub fn on_call(call_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let call = decode_call(call_bytes)?;
    let response = match call {
        WasmCall::EvaluatePolicy { policy, file, .. } => {
            let result = evaluate(&policy, &file)
                .map_err(|e| e.to_string())?;
            CallResponse::EvaluatePolicy(result)
        }
        other => return Err(format!("unhandled WasmCall: {other:?}")),
    };
    encode_response(&response)
}
```

The `wit_bindgen::generate!` + `export!` block that wires `get_info`,
`handles`, `on_event`, and `on_call` to the WIT `Guest` exports is the
final step; see the example plugin sources for the boilerplate.

### Streaming Calls (WASM)

For streaming variants (`WasmCall::ScanLibrary` today), emit items
through host imports rather than `mpsc::Sender` fields. The SDK exposes
typed wrappers:

```rust
use voom_plugin_sdk::{emit_file_discovered, is_cancelled};
use voom_plugin_sdk::host::HostFunctions;

fn scan(host: &dyn HostFunctions, uri: &str) -> Result<(), String> {
    for entry in walk(uri) {
        if is_cancelled(host) { break; }
        emit_file_discovered(host, &entry.into())?;
    }
    Ok(())
}
```

The underlying WIT contract — `on-call`, `emit-call-item`,
`emit-root-walk-completed`, `call-is-cancelled` — is defined in
`crates/voom-wit/wit/host.wit` and `crates/voom-wit/wit/plugin.wit`.

## Capabilities

Both tiers claim capabilities via the same
`voom_domain::capabilities::Capability` enum (re-exported from
`voom_plugin_sdk` for WASM authors):

- **Native** — return them from `Plugin::capabilities() -> &[Capability]`.
- **WASM** — pass them as the third argument to `PluginInfoData::new`
  inside your `get_info()` free function (the SDK forwards them through
  the WIT `plugin-info.capabilities` field).

Each capability has a resolution discipline
(`voom_domain::capability_resolution::CapabilityResolution`): `Exclusive`
(at most one claimant), `Sharded` (disjoint per-key claimants), or
`Competing` (kernel picks at dispatch time by priority). Claiming an
Exclusive capability that another plugin already claims is a
registration error; Sharded capabilities reject colliding shard keys.

## See also

- `docs/plugin-development.md` — full two-tier authoring walkthrough.
- `docs/architecture.md` — communication primitives, capability-based
  routing, and stats-sink instrumentation.
- `docs/superpowers/specs/2026-05-14-plugin-stats-self-reporting-design.md`
  — rev-6 design rationale.
