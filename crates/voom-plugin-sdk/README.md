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

WASM plugins do not implement `voom_kernel::Plugin`. They implement the
WIT-generated `Guest` trait — `wit_bindgen::generate!` produces it from
`crates/voom-wit/wit/plugin.wit` — and `export!(YourType)` wires that
impl to the WIT exports the host loads. There is no free-function
shortcut; `wit_bindgen` only exports methods on a type that implements
`Guest`. (The in-tree `wasm-plugins/*` crates are stub sketches that
demonstrate the SDK helper types but do not include the
`wit_bindgen::generate!` / `export!` lines and so do not produce loadable
`.wasm` artifacts as-is.)

The `Guest` trait has four methods that map directly to the WIT
contract:

| WIT export                                  | Guest method   | Returns |
| ------------------------------------------- | -------------- | --- |
| `get-info: func() -> plugin-info`           | `get_info`     | the WIT-generated `PluginInfo` record |
| `handles: func(event-type) -> bool`         | `handles`      | `bool` |
| `on-event: func(event-data) -> option<...>` | `on_event`     | `Option<EventResult>` |
| `on-call: func(list<u8>) -> result<...>`    | `on_call`      | `Result<Vec<u8>, String>` |

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
    // get-info: build the WIT-generated PluginInfo record directly.
    // `Capability` here is the WIT-generated enum (kebab-case names),
    // not voom_domain::capabilities::Capability.
    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "my-plugin".into(),
            version: "0.1.0".into(),
            description: Some("Example".into()),
            author: None,
            license: Some("MIT".into()),
            homepage: None,
            capabilities: vec![Capability::EvaluatePolicy],
        }
    }

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

`PluginInfoData` (re-exported from `voom_plugin_sdk::types`) is an
optional convenience builder you can use inside `get_info()` to assemble
a `PluginInfo` field-by-field — it is **not** itself the type
`Guest::get_info` returns.

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

Both tiers claim capabilities, but the enum used differs by tier:

- **Native** — return `&[voom_domain::capabilities::Capability]` from
  `Plugin::capabilities()`.
- **WASM** — list `Capability` values (the WIT-generated variant, with
  kebab-case variant names — `evaluate-policy`, `orchestrate-phases`,
  `enrich-metadata(...)`, etc., defined in
  `crates/voom-wit/wit/plugin.wit`) inside the `PluginInfo` returned by
  `Guest::get_info()`.

The host translates the WIT enum into the domain enum when it loads the
plugin, so a WASM plugin claiming `Capability::EvaluatePolicy` collides
with the same claim from a native plugin during registry validation.

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
