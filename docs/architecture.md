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
│     Native Plugins — Kernel-Registered (event bus dispatch)     │
│                                                                │
│   Discovery ────── Tool Detector ───── Storage                 │
│   MKVToolNix ───── FFmpeg ──────────── Backup                  │
│   Job Manager ──── Introspection ───── Bus Tracer              │
│   Health ───────── Capabilities ────── Report                  │
│                                                                │
│     Native Plugins — Library-Only (called directly by CLI)     │
│                                                                │
│   Evaluator ────── Orchestrator                                │
│   Web Server                                                   │
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

4. **Events for coordination** — Passive subscribers (storage, backup, SSE) communicate exclusively through the event bus. The CLI commands call the policy-evaluator and phase-orchestrator directly for deterministic progress reporting and concurrency control, dispatching lifecycle events (`PlanCreated`, `PlanExecuting`, etc.) for downstream subscribers.

5. **Domain types as lingua franca** — All plugins share types from `voom-domain`. WASM plugins access these types via WIT interfaces with MessagePack serialization at the boundary.

6. **Immutable data** — Domain types implement `Clone` but mutations produce new values.

## Workspace Crates

```
voom/
├── crates/
│   ├── voom-kernel/          # Event bus, plugin registry, native + WASM loader
│   ├── voom-domain/          # Shared types: MediaFile, Track, Plan, Event, Capability
│   ├── voom-dsl/             # PEG grammar (pest), parser, AST, compiler, validator, formatter
│   ├── voom-cli/             # clap-derive CLI binary with 20 subcommands
│   ├── voom-process/         # Shared subprocess utilities with timeout-aware execution
│   ├── voom-wit/             # WIT interface definitions + type conversion utilities
│   └── voom-plugin-sdk/      # SDK crate for WASM plugin authors
├── plugins/                  # Native plugins (compiled into binary)
│   ├── discovery/            # Filesystem walking (walkdir + rayon), content hashing (xxHash64)
│   ├── ffprobe-introspector/ # ffprobe JSON parsing, codec/HDR/VFR detection (kernel-registered)
│   ├── tool-detector/        # PATH lookup, version parsing for external tools
│   ├── sqlite-store/         # SQLite persistence (r2d2 pool, WAL mode)
│   ├── policy-evaluator/     # Track filtering, condition evaluation, Plan generation (library)
│   ├── phase-orchestrator/   # Phase sequencing with skip_when/depends_on/run_if (library)
│   ├── mkvtoolnix-executor/  # mkvpropedit + mkvmerge command builders
│   ├── ffmpeg-executor/      # FFmpeg command builder, HW accel, progress parsing
│   ├── backup-manager/       # File backup/restore with disk space validation
│   ├── job-manager/          # Priority queue, concurrent worker pool (tokio + Semaphore)
│   ├── bus-tracer/           # Event bus tracer — configurable event logging for development
│   ├── health-checker/       # Environment diagnostics
│   ├── report/               # Library analytics and report queries
│   ├── web-server/           # axum REST API + htmx/Alpine.js web UI + SSE (started by `serve`)
│   └── web-sse-bridge/       # Bridges event bus → SSE stream (registered when `serve` runs)
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
    fn description(&self) -> &str { "" }
    fn author(&self) -> &str { "" }
    fn license(&self) -> &str { "" }
    fn homepage(&self) -> &str { "" }
    fn capabilities(&self) -> &[Capability];
    fn handles(&self, _event_type: &str) -> bool { false }
    fn on_event(&self, _event: &Event) -> Result<Option<EventResult>> { Ok(None) }
    fn init(&mut self, _ctx: &PluginContext) -> Result<()> { Ok(()) }
    fn shutdown(&self) -> Result<()> { Ok(()) }
}
```

Plugins that participate in event-driven coordination override `handles()` and `on_event()`. Library-only plugins (policy-evaluator, phase-orchestrator, web-server) are called directly by the CLI and are not registered with the kernel — they don't participate in event dispatch.

The ffprobe-introspector is both kernel-registered (subscribes to `FileDiscovered` to enqueue introspection jobs) and called directly by the CLI (for deterministic progress reporting). The bus-tracer is a development tool that logs events to a file with configurable glob-pattern filtering.

### `verifier` vs `health-checker`

Two distinct integrity concepts share similar names but check different things:

- **`voom env check`** (plugin: `health-checker`) — environment readiness:
  ffmpeg / mkvtoolnix presence, GPU availability, data-directory writability,
  database connectivity. Run periodically by the serve loop.
- **`voom verify`** (plugin: `verifier`) — per-file media integrity:
  container header (quick), full decode (thorough), or sha256 bit-rot (hash).
  Persisted to the `verifications` table, reportable via
  `voom verify report` and `voom report --integrity`.

The two are intentionally orthogonal and never share a database table.

See [`docs/usage/verify.md`](usage/verify.md) for `voom verify` usage details.

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
    Execute { operations: Vec<String>, formats: Vec<String> },
    Store { backend: String },                // e.g., "sqlite"
    DetectTools,
    ManageJobs,
    ServeHttp,
    Plan,
    Backup,
    EnrichMetadata { source: String },        // e.g., "radarr", "sonarr"
    Transcribe,
    Synthesize,
}
```

Capabilities are used for plugin registration and discovery. Currently, executor routing uses priority-ordered event dispatch: when a `PlanCreated` event is published, the first executor that claims it (via `EventResult.claimed`) handles the plan. Capability-based routing is planned for a future sprint; the registry currently uses name-based lookup only.

## Event Bus

The event bus is the sole communication mechanism between plugins. It uses synchronous priority-ordered dispatch with `parking_lot::RwLock`.

### Event Types

| Event | Emitter | Description |
|-------|---------|-------------|
| `file.discovered` | Discovery | New file found during scan |
| `file.introspected` | CLI (introspect helper) | File metadata extracted via ffprobe |
| `file.introspection_failed` | CLI (introspect helper) | File introspection failed |
| `introspect.session.completed` | CLI (`process` command) | End of a standalone re-introspection batch (one per run); session-level, not per-file — for per-file see `file.introspected` |
| `scan.complete` | CLI (`scan` command) | End of a discovery + introspection scan; carries both totals |
| `metadata.enriched` | WASM plugins | External metadata added |
| `plan.created` | CLI (process command) | Execution plan dispatched for executor claiming |
| `plan.executing` | CLI (process command) | Plan execution about to start (triggers backup) |
| `plan.completed` | CLI (process command) | Plan execution succeeded |
| `plan.failed` | CLI (process command) | Plan execution failed or no executor claimed |
| `job.started` | Job Manager | Background job started |
| `job.progress` | Job Manager | Job progress update |
| `job.completed` | Job Manager | Job finished |
| `tool.detected` | Tool Detector | External tool found |

### Dispatch Model

Events are published to all subscribed plugins, ordered by priority (lower = runs first). Each subscriber can optionally return an `EventResult` that may influence downstream processing.

### Retention invariants

`event_log` records every event dispatched on the bus, including
`job.started` and `job.completed`. `jobs` records one row per work item the
worker pool processed.

For a single `voom process` run, the event_log gains roughly **seven rows
per job row**: file.discovered, file.introspected, three plan.* rows for
files that were transformed, plus job.started/completed. Default retention
must therefore keep `event_log` at *least* `7×` longer than `jobs`, on
both axes (`keep_last` and `keep_for_days`), or the event log will be
pruned while jobs survive — producing the misleading appearance that jobs
completed without announcing themselves on the bus.

The shipped defaults satisfy this: `jobs.keep_last = 50_000` and
`event_log.keep_last = 500_000`; `jobs.keep_for_days = 7` and
`event_log.keep_for_days = 60`. Operators who tighten `event_log` in
`config.toml` should keep the multiplier in mind — `voom env check`
warns when the oldest event is newer than the oldest job (issue #194).

## CLI Dual-Dispatch Pattern

CLI commands (`scan`, `process`) use a **hybrid approach**: direct plugin calls for CLI control flow, with event publishing for plugin coordination. This is an intentional architectural decision, not interim scaffolding.

### How it works

1. **Direct calls** drive the CLI workflow — discovery scanning, introspection, policy evaluation, and phase orchestration are called directly so the CLI controls progress reporting, error handling, and concurrency.
2. **Event publishing** notifies downstream plugins — after each direct call completes, the CLI dispatches the corresponding event (`FileIntrospected`, `PlanCreated`, `PlanExecuting`, etc.) through the kernel's event bus.
3. **Passive subscribers** (sqlite-store, backup-manager, web-server SSE, WASM metadata plugins) react to these events without the CLI needing to call them directly.

### Why not fully event-driven

Three CLI requirements cannot be satisfied by the current event bus:

- **Progress reporting** — Progress bars are driven by direct return values and worker pool callbacks (`on_job_start`, `on_job_complete`). The event bus is fire-and-forget with no feedback channel to the caller.
- **Error strategies** — `ErrorStrategy::Fail` cancels a `CancellationToken` to stop all workers immediately; `Skip`/`Continue` let the batch proceed. The event bus has no batch-level error strategy — a failed handler produces a `PluginError` event but cannot halt or skip processing.
- **Concurrency control** — The worker pool uses `tokio::Semaphore` to limit concurrent file processing. The event bus dispatches synchronously in priority order with no parallelism.

### Side-effect safety

There is no duplication of side effects. CLI commands never call storage methods (`upsert_file`, `save_plan`, `update_plan_status`) directly. All persistence flows exclusively through event dispatch to sqlite-store's `on_event` handler, ensuring a single write path.

### Per-command details

| Command | Direct calls | Events dispatched |
|---------|-------------|-------------------|
| `scan` | `discovery.scan()`, `introspect_file()` | `FileDiscovered`, `FileIntrospected`, `FileIntrospectionFailed` |
| `process` | `discovery.scan()`, `introspect_file()`, `evaluate()`, `orchestrate()` | `FileDiscovered`, `FileIntrospected`, `PlanExecuting`, `PlanCreated`, `PlanCompleted`/`PlanFailed` |

Both commands dispatch `FileDiscovered` events so sqlite-store records files in the `discovered_files` staging table and ffprobe-introspector enqueues introspection jobs. Introspection is still driven directly by the CLI for deterministic progress reporting; the enqueued jobs exist for future daemon-mode use.

### Scan sessions (issue #358, phase 1)

Scan reconciliation runs through three explicit primitives on `FileStorage`:
`begin_scan_session(roots)`, `ingest_discovered_file(session, file)`, and
`finish_scan_session(session)`. Per-file ingest makes the file visible in the
`files` table immediately and stamps `files.last_seen_session_id`. Missing-file
detection runs only at session finish, marking active files under the session's
roots whose `last_seen_session_id` is not the finishing session. An interrupted
session is auto-cancelled when the next `begin_scan_session` runs; no file is
ever marked missing without a successful `finish`. The legacy
`reconcile_discovered_files` is a thin wrapper around these primitives, so any
existing caller continues to work.

See `docs/superpowers/specs/2026-05-11-scan-sessions-design.md` and issue #358
for the full specification.

## Data Flow

```
DSL Policy File (.voom)              Media Files on Disk
      │                                     │
      ▼                                     ▼
  pest parser                        Discovery Plugin
  + compiler                         (rayon + walkdir)
      │                                     │
      ▼                              FileDiscovered events
  CompiledPolicy                       ┌────┴────┐
      │                                │         │
      │                          Storage      Introspector
      │                          Plugin       Plugin
      │                         (staging)   (enqueue job)
      │                                │
      │                         Introspection
      │                        (ffprobe, direct)
      │                                │
      │                        FileIntrospected events
      │                                │
      │                          Storage Plugin
      │                           (persist file)
      │                                │
      ▼                                ▼
  Phase Orchestrator ──── sequences phases, checks skip/run_if
      │
      ▼
  Policy Evaluator ───── matches tracks, evaluates conditions
      │
      ▼
  Plan (serializable, inspectable, approvable)
      │
      ▼
  Executor Plugin ────── priority-claimed (MKVToolNix or FFmpeg)
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

SQLite database in WAL mode with r2d2 connection pool. Tables: `files`, `tracks`, `jobs`, `plans`, `processing_stats`, `plugin_data`, `bad_files`, `discovered_files`. All domain types are serde-serializable (JSON + MessagePack). The `bad_files` table tracks files that failed introspection, with upsert semantics that increment `attempt_count` on repeated failures. The `discovered_files` table is a staging table that tracks files from discovery through introspection (statuses: `pending` → `introspecting` → `completed` | `failed`).

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
| Language | Rust 2024 edition |
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
