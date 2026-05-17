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
│   Phase Orchestrator ── Policy Evaluator                       │
│                                                                │
│     Native Plugins — Library-Only (started by `serve` command) │
│                                                                │
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

Plugins that participate in event-driven coordination override `handles()` and `on_event()`. Plugins that handle capability-routed RPCs override `on_call()` and return the relevant `Capability` from `capabilities()` — see [Communication primitives — Events vs Calls](#communication-primitives--events-vs-calls) and [Capability-based routing](#capability-based-routing) below. The `web-server` is the remaining library-only plugin: it is started by `voom serve` and is not registered with the kernel during one-shot CLI runs.

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

Capabilities are used for plugin registration and discovery. Currently, executor routing uses priority-ordered event dispatch: when a `PlanCreated` event is published, the first executor that claims it (via `EventResult.claimed`) handles the plan. Capability-based routing is now first-class for unary and streaming RPCs — see [Capability-based routing](#capability-based-routing) below. Executor selection for `PlanCreated` still uses priority-ordered event dispatch; converting the executors to capability-routed `on_call` is tracked separately.

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

### Bus dispatcher instrumentation

Every plugin invocation that flows through the kernel — whether the kernel
calls `Plugin::on_event` (event subscribers) or `Plugin::on_call` (unary and
streaming RPCs routed via `dispatch_to_capability`) — is timed and recorded as
one `PluginStatRecord`. The stats sink is kernel-internal; plugins have no
write API. Outcomes are classified `Ok` / `Err` / `Panic`. Coverage is
complete: pure publishers like `discovery` are now kernel-registered plugins
whose work happens inside `on_call(Call::ScanLibrary)`, and offline callees
like `policy-evaluator` and `phase-orchestrator` are kernel-registered with
`on_call(Call::EvaluatePolicy)` and `on_call(Call::Orchestrate)`
respectively. The default sink (`NoopStatsSink`) discards records; the CLI
wires `SqliteStatsSink` to persist to the `plugin_stats` table. Records are
written in background batches and dropped if the bounded channel overflows —
the bus must not block.

### Communication primitives — Events vs Calls

The kernel supports three primitives, each suited to a different interaction
shape:

**Event broadcast (pub/sub).** Plugins emit events via
`kernel.dispatch(Event::*)`. Every subscriber receives a copy in priority
order. The event author does not know — and cannot influence — who handles
it. Best for one-to-many notifications (`FileDiscovered`, `PlanCompleted`,
`JobStarted`). Subscribers implement `Plugin::on_event` and declare interest
via `Plugin::handles`.

**Unary Call.** A request-response RPC dispatched to whichever plugin claims
the matching `Capability`. The caller gets exactly one `CallResponse` (or an
error). Best for "I need *the* policy evaluator to evaluate this file":

```rust
let query = CapabilityQuery::Exclusive {
    kind: Capability::EvaluatePolicy.kind().to_string(),
};
let response = kernel.dispatch_to_capability(
    query,
    Call::EvaluatePolicy {
        policy: Box::new(compiled),
        file: Box::new(media_file),
        phase: None,
        phase_outputs: None,
        phase_outcomes: None,
        capabilities_override: None,
    },
)?;
let CallResponse::EvaluatePolicy(eval_result) = response else { unreachable!() };
```

Implemented in the plugin via `on_call`. `Plugin::on_call` takes `&Call`, so
the destructured variant fields are references; deref coercion lets them flow
into functions that take `&CompiledPolicy` / `&MediaFile`:

```rust
impl Plugin for PolicyEvaluatorPlugin {
    fn on_call(&self, call: &Call) -> voom_domain::errors::Result<CallResponse> {
        let Call::EvaluatePolicy { policy, file, .. } = call else {
            return Err(VoomError::plugin(
                self.name(),
                format!(
                    "PolicyEvaluatorPlugin only handles Call::EvaluatePolicy, got {:?}",
                    std::mem::discriminant(call)
                ),
            ));
        };
        let result = self.evaluate(policy, file)?;
        Ok(CallResponse::EvaluatePolicy(result))
    }
}
```

**Streaming Call.** A long-lived RPC that emits items through a host-provided
`mpsc::Sender` while running. Best when the producer's output is unbounded or
the consumer wants backpressure (`Call::ScanLibrary` streams
`FileDiscoveredEvent`s through `sink: mpsc::Sender<FileDiscoveredEvent>` so a
saturated CLI consumer blocks the plugin's `blocking_send`). Cancellation
flows the opposite direction via `CancellationToken`. WASM plugins use
`emit-call-item` (host import) to enqueue items.

**Why three?** Event broadcast loses the response. Unary Call doesn't fit
producers whose output is the work. Streaming Call adds the per-item channel
without the breadth-first cost of broadcast.

### Capability-based routing

`Kernel::dispatch_to_capability(query, call)` resolves the routing target at
call time rather than baking handler identity into the caller:

```rust
pub fn dispatch_to_capability(
    &self,
    query: CapabilityQuery,
    call: Call,
) -> Result<CallResponse>
```

Each `Capability` variant declares its resolution discipline via
`Capability::resolution()`:

- **Exclusive** — at most one registered plugin may claim it. Registration
  fails with a duplicate-claim error if two plugins both declare the same
  Exclusive capability. Used by `EvaluatePolicy`, `OrchestratePhases`, and
  `ScanLibrary`. The kernel routes the call to the single claimant.
- **Sharded** — multiple plugins claim disjoint shards (e.g. one executor per
  codec family). The kernel routes on a shard key the caller supplies.
- **Shared** — any plugin may claim; the caller picks one explicitly (rare;
  reserved for future use).

A plugin claims one or more capabilities by returning them from
`Plugin::capabilities()`. The trait returns `&[Capability]`, so plugins
typically store the claim list in a field set up by `new()`:

```rust
pub struct PhaseOrchestratorPlugin {
    capabilities: Vec<Capability>,
}

impl Plugin for PhaseOrchestratorPlugin {
    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}
```

The kernel enforces uniqueness at `Registry::register` and surfaces
violations as an actionable error with the colliding plugin names.
`Registry::get_typed::<P>()` is available for the rare case where a caller
wants the concrete plugin handle rather than a routed call (used internally
by some test fixtures).

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

**Concurrency and recovery:** Each scan session has a `last_heartbeat_at`
timestamp, bumped on every `ingest_discovered_file` call. `begin_scan_session`
auto-cancels in-progress sessions whose heartbeat is older than 60 seconds
(stale, crashed scans). A session with a recent heartbeat is treated as a
live concurrent scan; the new `begin_scan_session` call errors out rather
than corrupting the in-flight scan's state.

**Stub recovery:** A row whose `last_seen_session_id` points to a
non-completed (cancelled, in_progress, or unknown) session is treated as a
stub left behind by an interrupted scan. The next ingest at the same path
deletes the stub and re-processes from scratch — restoring move detection
across crash boundaries and ensuring previously-stub files get
re-introspected.

See `docs/superpowers/specs/2026-05-11-scan-sessions-design.md`,
`docs/superpowers/specs/2026-05-11-scan-session-hardening-design.md`, and
issue #358 for the full specification.

### Scan pipeline (streaming, phase 2)

`voom scan` runs three cooperating stages: a discovery task that walks the
filesystem (via `voom-discovery::scan_directory_streaming`), an ingest task
that holds the active scan session and writes per-file rows as events
arrive, and a bounded pool of ffprobe workers (sized by `--probe-workers`,
default = `min(num_cpus, sqlite_pool_size - 2)` with a floor of 1).
Backpressure flows from the probe pool through ingest back to discovery,
so memory usage stays bounded even on libraries with tens of thousands of
files. Cancellation is honoured at every stage; cancelled sessions never
mark files missing.

See `docs/superpowers/specs/2026-05-11-issue-359-scan-streaming-phase-2-design.md`
for the full design.

### Per-root execution gating (issue #361)

The streaming process pipeline uses a `RootGate` and a per-root
`HoldingBuffer<WorkItem>` to coordinate when each scanned root unlocks for
mutating execution.

- **Default behavior** (no `--execute-during-discovery` flag): every root
  must finish its filesystem walk before any root unlocks. This preserves
  the strict ordering of pre-issue-361 behavior. The `RootGate` and
  `HoldingBuffer` are not constructed in this mode.
- **Opt-in behavior** (`--execute-during-discovery` set): each root
  unlocks independently when its walk completes. Items for closed roots
  are held in the per-root `HoldingBuffer` and never enter the SQL `jobs`
  queue or the worker channel until their root's gate opens. A dispatcher
  task subscribes to `RootWalkCompleted` events, opens the gate for the
  named root, and drains the buffer in original priority order.

Three invariants make this safe across the inter-root window:

1. **Fail-closed mutation recording.** Before any visible filesystem
   write, executors call `record_voom_mutation`. If the storage write
   fails, the rename is not performed and the job fails — never the
   half-state "write happened but the record is missing."
2. **Preloaded mutation snapshot.** Before each root's walk begins, the
   pipeline loads a `SessionMutationSnapshot` from the active session's
   `scan_session_mutations` rows. The walker checks paths against this
   snapshot via an infallible `HashSet::contains`. If the snapshot
   cannot be loaded, the scan aborts.
3. **Gate at enqueue, not at claim.** SQL `jobs` rows are not created
   for items held in the `HoldingBuffer`. Workers therefore only ever
   see items that are already eligible to execute, eliminating the
   head-of-line blocking failure mode where high-priority closed-root
   items would otherwise occupy every worker slot.

VOOM-originated mutations are tagged with the active scan session id
(`scan_session_mutations` table) so the scanner skips them mid-walk and
`finish_scan_session` does not misclassify them as missing or externally
changed.

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
