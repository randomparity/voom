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

WASM plugins ship two artifacts side by side:

1. A compiled component (`my-plugin.wasm`) built with `wit-bindgen` against
   the WIT files at `crates/voom-wit/wit/`.
2. A manifest TOML (`my-plugin.toml`) that declares the plugin's identity,
   capability claims, subscribed events, and sandbox grants.

The host loads identity and capabilities **from the manifest, not from
any function exported by the WASM module.** `Plugin::name()` and
`Plugin::capabilities()` on the loaded `WasmPlugin` return the manifest's
values verbatim (`crates/voom-kernel/src/loader.rs`); the kernel does
not invoke `Guest::get_info` to read capabilities. If your manifest is
missing or its capability list is wrong, your plugin will not be routed
the corresponding Calls regardless of what its WIT exports say.

### Manifest TOML

The manifest sits alongside the `.wasm` file (`<plugin-name>.toml`).
Capability variants are the same `voom_domain::capabilities::Capability`
enum the native side uses; serde renders them as TOML tables under
`[[capabilities]]`:

```toml
name = "my-plugin"
version = "0.1.0"
description = "Example metadata enrichment plugin"
author = "Me"
license = "MIT"
homepage = "https://example.com/my-plugin"

# Events the plugin subscribes to. The host uses this list (not the
# WASM module's WIT handles export) to decide which events to forward
# into the module's on-event entry point.
handles_events = ["file.introspected"]

# Capability claims (drives Plugin::capabilities → routing for Calls).
[[capabilities]]
[capabilities.EnrichMetadata]
source = "my-plugin"

# Optional sandbox grants.
allowed_domains = ["api.example.com"]

# Optional priority (lower = runs first; defaults to 70).
priority = 70

# Optional: pin the SDK protocol version (currently 1).
protocol_version = 1
```

Unit-variant capabilities (`EvaluatePolicy`, `OrchestratePhases`,
`DetectTools`, `ManageJobs`, `ServeHttp`, `Backup`, `Transcribe`,
`Synthesize`) use the empty-table form `EvaluatePolicy = {}`. Variants
that carry data (`Discover { schemes }`, `Introspect { formats }`,
`Execute { operations, formats }`, `Store { backend }`,
`EnrichMetadata { source }`) use the nested-table form shown above. See
`docs/plugin-development.md` and the existing `wasm-plugins/*`
manifests for worked examples.

### WIT exports (the `.wasm` side)

`Cargo.toml` dependencies: `voom-plugin-sdk`, `wit-bindgen`. The runtime
contract is the WIT-generated `Guest` trait — `wit_bindgen::generate!`
produces it from `crates/voom-wit/wit/plugin.wit` — and `export!(YourType)`
wires the impl to the WIT exports the host loads. There is no
free-function shortcut; `wit_bindgen` only exports methods on a type
that implements `Guest`. (The in-tree `wasm-plugins/*` crates are stub
sketches that demonstrate the SDK helper types but do not include the
`wit_bindgen::generate!` / `export!` lines and so do not produce
loadable `.wasm` artifacts as-is.)

The `Guest` trait has four methods that map to the WIT contract. Only
two are invoked by the kernel today; the other two exist in the contract
but are satisfied entirely by the manifest TOML:

| WIT export                                  | Guest method   | What the host does with the return |
| ------------------------------------------- | -------------- | --- |
| `get-info: func() -> plugin-info`           | `get_info`     | not invoked; identity and capabilities come from the manifest |
| `handles: func(event-type) -> bool`         | `handles`      | not invoked; the per-event filter is `manifest.handles_events` |
| `on-event: func(event-data) -> option<...>` | `on_event`     | called for each event the manifest opted into; returns optional produced events |
| `on-call: func(list<u8>) -> result<...>`    | `on_call`      | called when the kernel routes a Call to this plugin; returns MessagePack `CallResponse` bytes or error string |

A minimal implementation that uses SDK helpers for the Call boundary:

```rust
use voom_plugin_sdk::{
    WasmCall, CallResponse, decode_call, encode_response,
};

wit_bindgen::generate!({
    world: "voom-plugin",
    path: "../../crates/voom-wit/wit",
});

struct MyPlugin;

impl Guest for MyPlugin {
    // get-info is part of the WIT contract; the kernel does not call it
    // today (capabilities come from the manifest). Returning a minimal
    // PluginInfo satisfies the trait without claiming anything extra.
    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "my-plugin".into(),
            version: "0.1.0".into(),
            description: None,
            author: None,
            license: None,
            homepage: None,
            capabilities: vec![],
        }
    }

    // handles is also unused by the kernel today; the per-event filter
    // is `manifest.handles_events`. Returning false here is harmless.
    fn handles(_event_type: String) -> bool { false }

    fn on_event(_event: EventData) -> Option<EventResult> { None }

    // on-call: decode the host bytes into a typed WasmCall, do the work,
    // encode the CallResponse. WasmCall omits the non-serde fields
    // (sink, root_done, cancel) — those are host-only.
    fn on_call(call_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let call = decode_call(&call_bytes)?;
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
}

export!(MyPlugin);
```

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

Both tiers claim capabilities against the same
`voom_domain::capabilities::Capability` enum, but declare them through
different mechanisms:

- **Native** — return `&[Capability]` from `Plugin::capabilities()`.
- **WASM** — list them under `[[capabilities]]` in the manifest TOML.
  The kernel loads the manifest at plugin-load time and uses those
  values as the plugin's capability set. Anything declared in
  `Guest::get_info` is ignored for routing today.

Because both tiers feed the same `Capability` enum into the registry, a
WASM plugin claiming `Capability::EvaluatePolicy` collides with the
same claim from a native plugin during registration.

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
