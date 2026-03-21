# VOOM Architecture Overview

VOOM (Video Orchestration Operations Manager) is a policy-driven video library manager built in Rust. It uses a plugin-first architecture with a thin kernel, a custom DSL for policy configuration, and supports both native and WASM plugins.

## Layer Diagram

```
┌────────────────────────────────────────────────────────────────┐
│                     Presentation Layer                         │
│   ┌─────────────────────┐    ┌──────────────────────────────┐  │
│   │    CLI (clap)       │    │  Web UI (axum + htmx)        │  │
│   └─────────────────────┘    └──────────────────────────────┘  │
├────────────────────────────────────────────────────────────────┤
│                       Core Kernel                              │
│   ┌────────────┐  ┌───────────┐  ┌────────────────────────┐    │
│   │  Event Bus │  │ Registry  │  │  Plugin Loader         │    │
│   │(sync/prio) │  │           │  │  (native + wasmtime)   │    │
│   └────────────┘  └───────────┘  └────────────────────────┘    │
├────────────────────────────────────────────────────────────────┤
│                      DSL Engine                                │
│   ┌────────┐ ┌────────┐ ┌──────────┐ ┌──────────┐ ┌───────┐    │
│   │ Lexer  │ │ Parser │ │ Compiler │ │Validator │ │Printer│    │
│   │ (pest) │ │ (pest) │ │          │ │          │ │       │    │
│   └────────┘ └────────┘ └──────────┘ └──────────┘ └───────┘    │
├────────────────────────────────────────────────────────────────┤
│            Native Plugins (compiled into binary)               │
│                                                                │
│   Discovery ────── Introspection ────── Storage                │
│   Evaluator ────── Orchestrator ─────── Jobs                   │
│   MKVToolNix ───── FFmpeg ──────────── Backup                  │
│   Web Server ───── Tool Detector                               │
├────────────────────────────────────────────────────────────────┤
│            WASM Plugins (loaded at runtime via wasmtime)       │
│                                                                │
│   Radarr ───────── Sonarr ──────────── Whisper                 │
│   TVDB ─────────── HandBrake ───────── Audio Synthesizer       │
├────────────────────────────────────────────────────────────────┤
│                   Domain Types (shared)                        │
│   MediaFile · Track · Plan · Action · Event · Capability       │
│   (serde-serializable, shared via WIT interface for WASM)      │
└────────────────────────────────────────────────────────────────┘
```

## Core Design Principles

1. **Kernel is inert** — The kernel has zero media knowledge. It manages plugin lifecycle, event dispatch, and capability routing only.

2. **Capabilities, not types** — Plugins declare capabilities (e.g., `Execute { ops: [transcode], formats: [mkv] }`). The kernel routes work by matching required capabilities to available plugins.

3. **Plan as contract** — The policy evaluator produces serializable `Plan` structs describing what to do. Executors consume them. Plans can be inspected, approved, and audited before execution.

4. **Events for coordination** — All inter-plugin communication happens through the event bus. No plugin directly calls another.

5. **Domain types as lingua franca** — All plugins share types from `voom-domain`. WASM plugins access these types via WIT interfaces with MessagePack serialization at the boundary.

6. **Immutable data** — Domain types implement `Clone` but mutations produce new values.

## Workspace Crates

```
voom/
├── crates/
│   ├── voom-kernel/          # Event bus, plugin registry, native + WASM loader
│   ├── voom-domain/          # Shared types: MediaFile, Track, Plan, Event, Capability
│   ├── voom-dsl/             # PEG grammar (pest), parser, AST, compiler, validator, formatter
│   ├── voom-cli/             # clap-derive CLI binary with 14 subcommands
│   ├── voom-wit/             # WIT interface definitions + type conversion utilities
│   └── voom-plugin-sdk/      # SDK crate for WASM plugin authors
├── plugins/                  # Native plugins (compiled into binary)
│   ├── discovery/            # Filesystem walking (walkdir + rayon), content hashing (xxHash64)
│   ├── ffprobe-introspector/ # ffprobe JSON parsing, codec/HDR/VFR detection
│   ├── tool-detector/        # PATH lookup, version parsing for external tools
│   ├── sqlite-store/         # SQLite persistence (r2d2 pool, WAL mode)
│   ├── policy-evaluator/     # Track filtering, condition evaluation, Plan generation
│   ├── phase-orchestrator/   # Phase sequencing with skip_when/depends_on/run_if
│   ├── mkvtoolnix-executor/  # mkvpropedit + mkvmerge command builders
│   ├── ffmpeg-executor/      # FFmpeg command builder, HW accel, progress parsing
│   ├── backup-manager/       # File backup/restore with disk space validation
│   ├── job-manager/          # Priority queue, concurrent worker pool (tokio + Semaphore)
│   └── web-server/           # axum REST API + htmx/Alpine.js web UI + SSE
└── wasm-plugins/             # WASM plugins (excluded from workspace, target wasm32)
    ├── example-metadata/     # Example plugin demonstrating the SDK
    ├── radarr-metadata/      # Movie metadata enrichment via Radarr API
    ├── sonarr-metadata/      # TV metadata enrichment via Sonarr API
    ├── tvdb-metadata/        # TV metadata enrichment from TVDB API
    ├── whisper-transcriber/   # Audio transcription via Whisper
    ├── audio-synthesizer/    # Audio track synthesis
    └── handbrake-executor/   # HandBrakeCLI-based transcoding
```

## Two-Tier Plugin Model

### Native Plugins

- Compiled directly into the binary as Rust crates
- Zero overhead — direct function calls via trait objects (`Arc<dyn Plugin>`)
- Full access to the Rust ecosystem
- Used for performance-critical core functionality
- All implement the `Plugin` trait:

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
    fn handles(&self, event_type: &str) -> bool;
    fn on_event(&self, event: &Event) -> Result<Option<EventResult>>;
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> { Ok(()) }
    fn shutdown(&self) -> Result<()> { Ok(()) }
}
```

### WASM Plugins

- Compiled to WebAssembly (`wasm32-wasi`), loaded at runtime via wasmtime 29
- Sandboxed execution — cannot directly access filesystem or network
- Language-agnostic — write in any language that compiles to WASM (Rust, Go, C, Zig)
- Communicate with host via WIT (WebAssembly Interface Types)
- Host provides sandboxed capabilities: file metadata, tool invocation, key-value storage, HTTP, logging
- Slight MessagePack serialization overhead at the boundary
- Manifests are TOML files alongside `.wasm` files

## Capability System

Plugins declare capabilities using the `Capability` enum. The kernel matches required capabilities to available plugins for routing:

```rust
pub enum Capability {
    Discover { schemes: Vec<String> },        // e.g., ["file", "smb"]
    Introspect { formats: Vec<String> },      // e.g., ["mkv", "mp4", "avi"]
    Evaluate,
    Execute { operations: Vec<String>, formats: Vec<String> },
    Store { backend: String },                // e.g., "sqlite"
    DetectTools,
    ManageJobs,
    ServeHttp,
    Orchestrate,
    Backup,
    EnrichMetadata { source: String },        // e.g., "radarr", "sonarr"
    Transcribe,
    Synthesize,
}
```

When the kernel needs to route an operation, it finds all plugins with matching capabilities and selects the best match (lowest priority value wins).

## Event Bus

The event bus is the sole communication mechanism between plugins. It uses synchronous priority-ordered dispatch with `parking_lot::RwLock`.

### Event Types

| Event | Emitter | Description |
|-------|---------|-------------|
| `file.discovered` | Discovery | New file found during scan |
| `file.introspected` | Introspector | File metadata extracted |
| `file.introspection_failed` | CLI (scan/process) | File introspection failed |
| `metadata.enriched` | WASM plugins | External metadata added |
| `policy.evaluate` | Orchestrator | Request policy evaluation |
| `plan.created` | Evaluator | Execution plan generated |
| `plan.executing` | Executor | Plan execution started |
| `plan.completed` | Executor | Plan execution succeeded |
| `plan.failed` | Executor | Plan execution failed |
| `job.started` | Job Manager | Background job started |
| `job.progress` | Job Manager | Job progress update |
| `job.completed` | Job Manager | Job finished |
| `tool.detected` | Tool Detector | External tool found |

### Dispatch Model

Events are published to all subscribed plugins, ordered by priority (lower = runs first). Each subscriber can optionally return an `EventResult` that may influence downstream processing.

## Data Flow

```
DSL Policy File (.voom)              Media Files on Disk
      │                                     │
      ▼                                     ▼
  pest parser                        Discovery Plugin
  + compiler                         (rayon + walkdir)
      │                                     │
      ▼                              FileDiscovered events
  CompiledPolicy                            │
      │                              Introspector Plugin
      │                              (ffprobe JSON parsing)
      │                                     │
      │                              FileIntrospected events
      │                                     │
      │                              Storage Plugin (SQLite)
      │                                     │
      ▼                                     ▼
  Phase Orchestrator ──── sequences phases, checks skip/run_if
      │
      ▼
  Policy Evaluator ───── matches tracks, evaluates conditions
      │
      ▼
  Plan (serializable, inspectable, approvable)
      │
      ▼
  Executor Plugin ────── capability-routed (MKVToolNix or FFmpeg)
      │
      ▼
  Modified media file
```

## Domain Model

### Core Types

- **`MediaFile`** — Represents a media file with path, size, content hash, container format, duration, tracks, tags, and plugin metadata.
- **`Track`** — Individual stream within a media file (video, audio, subtitle, attachment) with codec, language, channel info, resolution, HDR/VFR flags.
- **`TrackType`** — Classified track type: `Video`, `AudioMain`, `AudioAlternate`, `AudioCommentary`, `SubtitleMain`, `SubtitleForced`, etc.
- **`Plan`** — Serializable execution plan linking a file + policy + phase to a list of `PlannedAction`s.
- **`PlannedAction`** — Single operation (e.g., `SetDefault`, `RemoveTrack`, `TranscodeVideo`) with parameters.
- **`BadFile`** — A file that failed introspection, with error details, attempt count, and timestamps.
- **`Event`** — Tagged union of all event types for inter-plugin communication.
- **`Capability`** — What a plugin can do, used for routing.

### Storage

SQLite database in WAL mode with r2d2 connection pool. Tables: `files`, `tracks`, `jobs`, `plans`, `processing_stats`, `plugin_data`, `bad_files`. All domain types are serde-serializable (JSON + MessagePack). The `bad_files` table tracks files that failed introspection, with upsert semantics that increment `attempt_count` on repeated failures.

## DSL Pipeline

Policy files use `.voom` extension and a custom curly-brace block syntax:

```
Source (.voom) → pest parser → CST → AST builder → PolicyAst
    → Validator (semantic checks) → Compiler → CompiledPolicy
```

The validator catches: unknown codecs (with did-you-mean suggestions), circular phase dependencies, unreachable phases, conflicting actions, and invalid language codes.

See [DSL Language Reference](dsl-reference.md) for the complete language specification.

## Web UI

The web server plugin provides:
- **REST API** at `/api/*` — JSON endpoints for files, jobs, plugins, stats, policy validation/formatting
- **Web pages** — Dashboard, library browser, file detail, policy editor, job monitor, plugin manager, settings
- **SSE** at `/events` — Server-Sent Events for live updates (scan progress, job status)
- **Security** — CSP headers, optional token-based auth, X-Frame-Options, X-Content-Type-Options

Built with axum 0.7, Tera templates, htmx for server-driven updates, and Alpine.js for client-side state. Dark-themed UI with no build step required.

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust 2021 edition |
| CLI | clap (derive) |
| Web server | axum 0.7 + tower + tokio |
| Web frontend | htmx + Alpine.js + Tera templates |
| Database | SQLite (rusqlite + r2d2) |
| Config | TOML (serde) |
| DSL parser | pest (PEG) |
| WASM runtime | wasmtime 29 (component model) |
| Serialization | serde + rmp-serde (MessagePack) |
| Hashing | xxHash64 |
| Logging | tracing |
| Testing | built-in + insta (snapshots) + assert_cmd (CLI) + axum-test (API) |
| Error handling | thiserror + anyhow |
| File walking | walkdir + rayon |

## Configuration

| Item | Location |
|------|----------|
| App config | `~/.config/voom/config.toml` |
| Plugin data | `~/.config/voom/plugins/<name>/` |
| WASM plugins | `~/.config/voom/plugins/wasm/` |
| Database | `~/.config/voom/voom.db` (SQLite) |
| Policy files | Any `.voom` file |
