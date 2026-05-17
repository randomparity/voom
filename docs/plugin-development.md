# VOOM Plugin Development Guide

VOOM has a two-tier plugin architecture: **native plugins** compiled into the binary as Rust crates, and **WASM plugins** loaded at runtime via wasmtime. This guide covers developing both types.

## Plugin Concepts

### Plugin Trait

All plugins implement the `Plugin` trait from `voom-kernel`:

```rust
pub trait Plugin: Send + Sync {
    /// Unique plugin name.
    fn name(&self) -> &str;

    /// Semantic version string.
    fn version(&self) -> &str;

    /// Human-readable description of what this plugin does.
    fn description(&self) -> &str { "" }

    /// Plugin author(s).
    fn author(&self) -> &str { "" }

    /// License identifier (e.g., "MIT", "Apache-2.0").
    fn license(&self) -> &str { "" }

    /// Project homepage or repository URL.
    fn homepage(&self) -> &str { "" }

    /// Declared capabilities (used for routing).
    fn capabilities(&self) -> &[Capability];

    /// Does this plugin handle the given event type?
    fn handles(&self, event_type: &str) -> bool;

    /// Process an event and optionally return a result.
    fn on_event(&self, event: &Event) -> Result<Option<EventResult>>;

    /// Optional: Called once when the plugin is loaded.
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> { Ok(()) }

    /// Optional: Called when the application is shutting down.
    fn shutdown(&self) -> Result<()> { Ok(()) }
}
```

The `description`, `author`, `license`, and `homepage` methods have default implementations that return empty strings. Override them to provide metadata visible in `voom plugin list`, `voom plugin info`, and the web UI.

### Capabilities

Plugins declare what they can do using the `Capability` enum. The kernel uses capabilities to route work to the right plugin.

```rust
pub enum Capability {
    Discover { schemes: Vec<String> },
    Introspect { formats: Vec<String> },
    Evaluate,
    Execute { operations: Vec<String>, formats: Vec<String> },
    Store { backend: String },
    DetectTools,
    ManageJobs,
    ServeHttp,
    Orchestrate,
    Backup,
    EnrichMetadata { source: String },
    Transcribe,
    Synthesize,
}
```

### Event Types

Plugins communicate exclusively through events. A plugin declares which event types it handles, and the kernel dispatches matching events to it.

| Event Type | Description | Typical Handler |
|------------|-------------|-----------------|
| `file.discovered` | New file found during scan | Introspector |
| `file.introspected` | File metadata extracted | Storage, Enrichment |
| `metadata.enriched` | External metadata added | Storage |
| `policy.evaluate` | Request policy evaluation | Policy Evaluator |
| `plan.created` | Execution plan generated | Executor |
| `plan.executing` | Execution in progress | Job Manager |
| `plan.completed` | Execution succeeded | Storage |
| `plan.failed` | Execution failed | Job Manager |
| `job.started` | Background job started | Web Server (SSE) |
| `job.progress` | Job progress update | Web Server (SSE) |
| `job.completed` | Job finished | Web Server (SSE) |
| `tool.detected` | External tool found | — |

### Plugin Context

Plugins receive a `PluginContext` during initialization:

```rust
pub struct PluginContext {
    pub config: serde_json::Value,  // Plugin-specific config as a JSON value
    pub data_dir: PathBuf,          // Base data directory (e.g., ~/.config/voom/)
}
```

For logging, use `tracing` macros directly (`tracing::info!`, `tracing::debug!`, etc.) — there is no scoped logger in the context.

> **Note:** `PluginContext` applies to native plugins only. WASM plugins receive context via host functions (see [Host Functions](#host-functions) below).

---

## Native Plugin Development

Native plugins are Rust crates compiled directly into the VOOM binary. They have zero-overhead function calls and full access to the Rust ecosystem.

### Project Setup

Create a new crate in the `plugins/` directory:

```
plugins/
└── my-plugin/
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

**`Cargo.toml`:**

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2024"

[dependencies]
voom-kernel = { path = "../../crates/voom-kernel" }
voom-domain = { path = "../../crates/voom-domain" }
anyhow = "1"
tracing = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Add the crate to the workspace in the root `Cargo.toml`:

```toml
[workspace]
members = [
    # ...
    "plugins/my-plugin",
]
```

### Implementation

**`src/lib.rs`:**

```rust
use voom_domain::capabilities::Capability;
use voom_domain::errors::Result;
use voom_domain::events::{Event, EventResult};
use voom_kernel::{Plugin, PluginContext};

pub struct MyPlugin {
    capabilities: Vec<Capability>,
}

impl MyPlugin {
    pub fn new() -> Self {
        Self {
            capabilities: vec![
                Capability::EnrichMetadata {
                    source: "my-source".into(),
                },
            ],
        }
    }
}

impl Default for MyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for MyPlugin {
    fn name(&self) -> &str {
        "my-plugin"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn description(&self) -> &str {
        env!("CARGO_PKG_DESCRIPTION")
    }

    fn license(&self) -> &str {
        env!("CARGO_PKG_LICENSE")
    }

    fn homepage(&self) -> &str {
        env!("CARGO_PKG_REPOSITORY")
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn handles(&self, event_type: &str) -> bool {
        event_type == "file.introspected"
    }

    fn on_event(&self, event: &Event) -> Result<Option<EventResult>> {
        match event {
            Event::FileIntrospected(e) => {
                tracing::info!(path = %e.file.path.display(), "processing file");
                // Process the file and optionally return a result...
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn init(&mut self, _ctx: &PluginContext) -> Result<()> {
        tracing::info!("my-plugin initialized");
        Ok(())
    }
}
```

### Registration

Register your plugin in the kernel bootstrap (`crates/voom-cli/src/app.rs`):

```rust
use my_plugin::MyPlugin;

// In the bootstrap function:
kernel.init_and_register(Arc::new(MyPlugin::new()), 100, &ctx)?;
```

The `init_and_register` method calls `Plugin::init()` then registers the plugin with the given priority. As a lower-level alternative, `register_plugin(plugin, priority)` skips initialization.

### Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = MyPlugin::new();
        assert_eq!(plugin.name(), "my-plugin");
        assert!(!plugin.capabilities().is_empty());
    }

    #[test]
    fn test_handles_correct_events() {
        let plugin = MyPlugin::new();
        assert!(plugin.handles("file.introspected"));
        assert!(!plugin.handles("file.discovered"));
    }

    #[test]
    fn test_on_event() {
        let plugin = MyPlugin::new();
        // Create a test event and verify behavior...
    }
}
```

---

## WASM Plugin Development

WASM plugins are compiled to WebAssembly and loaded at runtime. They run in a sandboxed environment and communicate with the host through defined interfaces (WIT).

### Prerequisites

```bash
# Install the WASM target
rustup target add wasm32-wasip1
```

### Project Setup

Create a new crate (outside the workspace since it targets wasm32):

```
wasm-plugins/
└── my-wasm-plugin/
    ├── Cargo.toml
    └── src/
        └── lib.rs
```

**`Cargo.toml`:**

```toml
[package]
name = "my-wasm-plugin"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
voom-plugin-sdk = { path = "../../crates/voom-plugin-sdk" }
wit-bindgen = "0.36"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Exclude from the workspace in the root `Cargo.toml`:

```toml
[workspace]
exclude = [
    "wasm-plugins/my-wasm-plugin",
]
```

### WIT Interface

WASM plugins implement the `voom:plugin@0.3.0` world defined in `crates/voom-wit/wit/`:

**`world.wit`** — The plugin world:
```wit
package voom:plugin@0.3.0;

world voom-plugin {
    import host;    // Host functions the plugin can call
    export plugin;  // Functions the plugin must implement
}
```

**`plugin.wit`** — What plugins must implement:
```wit
interface plugin {
    use types.{event-data, event-result};

    record plugin-info {
        name: string,
        version: string,
        description: option<string>,
        author: option<string>,
        license: option<string>,
        homepage: option<string>,
        capabilities: list<capability>,
    }

    variant capability {
        discover(discover-cap),
        introspect(introspect-cap),
        evaluate,
        execute(execute-cap),
        enrich-metadata(enrich-cap),
        transcribe,
        synthesize,
    }

    get-info: func() -> plugin-info;
    handles: func(event-type: string) -> bool;
    on-event: func(event: event-data) -> option<event-result>;
}
```

**`host.wit`** — Host functions available to plugins:
```wit
interface host {
    // File operations (sandboxed to media library paths)
    read-file-metadata: func(path: string) -> result<media-file, string>;
    list-files: func(filters: file-filters) -> result<list<media-file>, string>;

    // Tool invocation (host executes the tool on behalf of the plugin)
    run-tool: func(tool: string, args: list<string>, timeout-ms: u64) -> result<tool-output, string>;

    // Persistent key-value storage
    get-plugin-data: func(key: string) -> option<list<u8>>;
    set-plugin-data: func(key: string, value: list<u8>) -> result<_, string>;

    // HTTP requests (for API access)
    http-get: func(url: string, headers: list<header>) -> result<http-response, string>;
    http-post: func(url: string, headers: list<header>, body: list<u8>) -> result<http-response, string>;

    // Logging
    log: func(level: log-level, message: string);
}
```

### Implementation

> **Capability and identity routing for WASM plugins comes from the
> manifest TOML, not from `Guest::get_info`.** The kernel loads
> `name`, `version`, the capability claim list, and the
> `handles_events` filter from the `<plugin-name>.toml` file that
> sits beside the `.wasm` artifact (`crates/voom-kernel/src/loader.rs`).
> `Guest::get_info` and `Guest::handles` are part of the WIT contract
> but the kernel does not call them today — they exist for forward
> compatibility. Keep the manifest as your source of truth; treat the
> `PluginInfo` returned from `get_info` as a stub.

**`src/lib.rs`:**

```rust
use voom_plugin_sdk::{deserialize_event, serialize_event, Event};

// Generate WIT bindings
wit_bindgen::generate!({
    world: "voom-plugin",
    path: "../../crates/voom-wit/wit",
});

struct MyWasmPlugin;

impl Guest for MyWasmPlugin {
    // The kernel does not invoke `get_info`. Identity and capabilities
    // are loaded from the manifest TOML below. Return a minimal stub.
    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "my-wasm-plugin".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            license: None,
            homepage: None,
            capabilities: vec![],
        }
    }

    // Not invoked by the kernel today — the per-event filter is
    // `manifest.handles_events`. Returning false here is harmless.
    fn handles(_event_type: String) -> bool {
        false
    }

    fn on_event(event: EventData) -> Option<EventResult> {
        // Deserialize the event from MessagePack bytes
        let domain_event = deserialize_event(&event.payload).ok()?;

        match &domain_event {
            Event::FileIntrospected(introspected) => {
                let file = &introspected.file;

                // Use host functions
                host::log(LogLevel::Info, &format!("Processing: {}", file.path.display()));

                // Example: Call an external API via host HTTP
                // let response = host::http_get(
                //     "https://api.example.com/lookup",
                //     &[Header { name: "Authorization".into(), value: "Bearer ...".into() }],
                // ).ok()?;

                // Example: Store plugin data
                // host::set_plugin_data("last-processed", file.path.to_string_lossy().as_bytes()).ok()?;

                // Create enrichment metadata
                let metadata = serde_json::json!({
                    "source": "my-wasm-plugin",
                    "custom_field": "custom_value",
                });

                let enriched = Event::MetadataEnriched(
                    voom_plugin_sdk::MetadataEnrichedEvent {
                        path: file.path.clone(),
                        source: "my-wasm-plugin".to_string(),
                        metadata,
                    },
                );

                let payload = serialize_event(&enriched).ok()?;

                Some(EventResult {
                    plugin_name: "my-wasm-plugin".to_string(),
                    produced_events: vec![EventData {
                        event_type: enriched.event_type().to_string(),
                        payload,
                    }],
                    data: None,
                })
            }
            _ => None,
        }
    }
}

export!(MyWasmPlugin);
```

### Plugin Manifest

Create a TOML manifest file alongside your `.wasm` binary:

**`my-wasm-plugin.toml`:**

```toml
name = "my-wasm-plugin"
version = "0.1.0"
description = "My custom WASM plugin"
author = "Your Name"
license = "MIT"
homepage = "https://github.com/you/my-wasm-plugin"
handles_events = ["file.introspected"]

[[capabilities]]
[capabilities.EnrichMetadata]
source = "my-source"
```

The manifest follows the `PluginManifest` struct:

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | yes | Unique plugin identifier |
| `version` | string | yes | Semantic version |
| `description` | string | yes | Human-readable description |
| `author` | string | no | Plugin author(s) |
| `license` | string | no | License identifier (e.g., "MIT") |
| `homepage` | string | no | Project homepage or repository URL |
| `handles_events` | list of strings | yes | Event types this plugin subscribes to |
| `capabilities` | list | yes | Declared capabilities |
| `dependencies` | list | no | Required plugins (name + version) |
| `config_schema` | JSON | no | JSON Schema for plugin config |

### Building

```bash
cd wasm-plugins/my-wasm-plugin
cargo build --target wasm32-wasip1 --release

# Output: target/wasm32-wasip1/release/my_wasm_plugin.wasm
```

### Installation

```bash
# Copy the .wasm and .toml files to the plugin directory
cp target/wasm32-wasip1/release/my_wasm_plugin.wasm ~/.config/voom/plugins/wasm/
cp my-wasm-plugin.toml ~/.config/voom/plugins/wasm/

# Or use the CLI
voom plugin install target/wasm32-wasip1/release/my_wasm_plugin.wasm
```

### Testing

Test your plugin logic without the WASM boundary by writing standard Rust tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use voom_plugin_sdk::*;
    use std::path::PathBuf;

    fn make_test_file() -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from("/test.mkv"));
        file.container = Container::Mkv;
        file.tracks = vec![
            Track::new(0, TrackType::Video, "hevc".into()),
        ];
        file
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("plan.created"));
    }

    #[test]
    fn test_on_event() {
        let file = make_test_file();
        let event = Event::FileIntrospected(
            voom_domain::events::FileIntrospectedEvent { file }
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload);
        assert!(result.is_some());
    }
}
```

---

## SDK Reference

The `voom-plugin-sdk` crate provides helpers for WASM plugins:

### Re-exported Types

```rust
// Domain types
pub use voom_domain::capabilities::Capability;
pub use voom_domain::events::{Event, EventResult};
pub use voom_domain::media::{Container, MediaFile, Track, TrackType};
pub use voom_domain::plan::{
    ActionResult, OperationType, PhaseOutcome, PhaseResult, Plan, PlannedAction,
};

// Full domain crate access
pub use voom_domain;
```

### Serialization Helpers

```rust
/// Deserialize an Event from MessagePack bytes (host → plugin).
pub fn deserialize_event(payload: &[u8]) -> Result<Event>;

/// Serialize an Event to MessagePack bytes (plugin → host).
pub fn serialize_event(event: &Event) -> Result<Vec<u8>>;

/// Deserialize any type from JSON bytes.
pub fn deserialize_json<T: DeserializeOwned>(data: &[u8]) -> Result<T>;

/// Serialize any type to JSON bytes.
pub fn serialize_json<T: Serialize>(value: &T) -> Result<Vec<u8>>;

/// Load plugin config from the host's plugin data store.
/// Pass a closure that calls `host::get_plugin_data`.
pub fn load_plugin_config<T: DeserializeOwned>(
    get_data: impl FnOnce(&str) -> Result<Option<Vec<u8>>, String>,
) -> Result<Option<T>>;
```

Usage in a WASM plugin:

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct MyConfig {
    api_key: String,
    poll_interval_secs: u64,
}

let config: Option<MyConfig> = match load_plugin_config(|key| host::get_plugin_data(key)) {
    Ok(config) => config,
    Err(e) => {
        host::log("error", &format!("failed to load plugin config: {e}"));
        return None;
    }
};
```

### PluginInfo Types

There are three `PluginInfo`-like types to be aware of:

- **WIT-generated `PluginInfo`** — The plain record struct generated by `wit_bindgen::generate!`. Used inside `impl Guest` for `get_info()`. Fields: `name`, `version`, `description` (option), `author` (option), `license` (option), `homepage` (option), `capabilities`.
- **`types::PluginInfo`** — The SDK's builder type with `.capability()` and `.handles()` chain methods. Useful for non-WIT contexts (e.g., testing). Not re-exported at the crate root to avoid colliding with the WIT-generated type.
- **`PluginInfoData`** — A lightweight mirror of the WIT record, re-exported at the crate root. Use `PluginInfoData::new(name, version, capabilities)` with builder methods `.with_description()`, `.with_author()`, `.with_license()`, `.with_homepage()` for optional metadata.

### Data Flow

```
Host                              WASM Plugin
  │                                    │
  │  EventData { event_type, payload } │
  │ ──────────────────────────────────►│
  │    (payload = MessagePack bytes)   │
  │                                    │  deserialize_event(payload)
  │                                    │  → Event (domain type)
  │                                    │
  │                                    │  ... process ...
  │                                    │
  │                                    │  serialize_event(&new_event)
  │  Option<EventResult>               │  → Vec<u8>
  │ ◄──────────────────────────────────│
  │                                    │
```

---

## Host Functions

WASM plugins can call host functions for sandboxed access to the system:

### File Operations

```wit
// Read metadata for a specific file
read-file-metadata(path: string) -> result<media-file, string>

// List files matching filters (extension, size range)
list-files(filters: file-filters) -> result<list<media-file>, string>
```

File access is sandboxed to configured media library paths.

### Tool Invocation

```wit
// Run an external tool (ffprobe, ffmpeg, mkvmerge, etc.)
run-tool(tool: string, args: list<string>, timeout-ms: u64) -> result<tool-output, string>
```

Only tools on the host's allowed-list can be invoked. The host executes the tool and returns stdout/stderr/exit-code.

### Persistent Storage

```wit
// Key-value storage scoped to the plugin
get-plugin-data(key: string) -> option<list<u8>>
set-plugin-data(key: string, value: list<u8>) -> result<_, string>
```

Data is persisted in the SQLite database, scoped per plugin name.

### HTTP

```wit
// HTTP GET and POST for API access
http-get(url: string, headers: list<header>) -> result<http-response, string>
http-post(url: string, headers: list<header>, body: list<u8>) -> result<http-response, string>
```

Used by metadata enrichment plugins to call external APIs (Radarr, Sonarr, TMDb, etc.).

### Logging

```wit
// Log at trace, debug, info, warn, or error level
log(level: log-level, message: string)
```

Messages appear in the host's tracing output, scoped to the plugin name.

---

## Capability Strings (WASM Boundary)

For the WASM boundary, capabilities are encoded as colon-separated strings:

| Capability | String Format | Example |
|------------|---------------|---------|
| Discover | `discover:<schemes>` | `discover:file,smb` |
| Introspect | `introspect:<formats>` | `introspect:mkv,mp4,avi` |
| Evaluate | `evaluate` | `evaluate` |
| Execute | `execute:<ops>:<formats>` | `execute:transcode+mux:mkv,mp4` |
| EnrichMetadata | `enrich_metadata:<source>` | `enrich_metadata:radarr` |
| Transcribe | `transcribe` | `transcribe` |
| Synthesize | `synthesize` | `synthesize` |

> **Casing note:** Capability string format uses underscores (e.g., `enrich_metadata`), matching Rust `Capability::kind()`. WIT interface names use kebab-case per WIT convention (e.g., `enrich-metadata`).

---

## Best Practices

1. **Declare precise capabilities** — Only declare capabilities you actually implement. The kernel uses these for routing.

2. **Handle events efficiently** — Return `None` quickly for events you don't care about. Check `event_type` before deserializing the payload.

3. **Use the SDK helpers** — Don't manually serialize/deserialize MessagePack. Use `deserialize_event()` and `serialize_event()`.

4. **Log at appropriate levels** — Use `tracing::info!` (native) or `host::log(LogLevel::Info, ...)` (WASM) for important operations. Use `debug` for details.

5. **Test without WASM** — Write your plugin logic as regular Rust functions and test them with standard `#[test]`. Only the WIT bindings require the WASM target.

6. **Keep manifests accurate** — The manifest's `handles_events` must match what your `handles()` function returns. The manifest is read before the plugin is loaded.

7. **WASM plugins are sandboxed** — You cannot access the filesystem, network, or environment directly. All external access goes through host functions.

8. **Produce events, don't call plugins** — Inter-plugin communication is via events only. Emit events and let the kernel route them.

---

## Related docs

- [architecture.md](architecture.md) — Two-tier plugin model, event bus, capability routing
- [dsl-reference.md](dsl-reference.md) — DSL policy language that drives plan creation
- [cli-reference.md](cli-reference.md) — `voom plugin` subcommands for managing plugins
