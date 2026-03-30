---
name: VOOM Event Bus Architecture
description: Event bus implementation, channel type, dispatch semantics, cascade depth, priority ordering, panic handling (verified on feat/address-cli-gaps-1)
type: project
---

**Bus type:** Synchronous, sequential dispatch (`parking_lot::RwLock<Vec<Subscriber>>`). NOT a tokio broadcast channel. Events are dispatched in priority order, one subscriber at a time.

**Location:** `crates/voom-kernel/src/bus.rs`

**Key behaviors:**
- `catch_unwind` wraps every `on_event()` call — panics produce a `PluginError` event, don't crash bus
- `MAX_CASCADE_DEPTH = 8` — cascaded events from `produced_events` are recursively dispatched up to depth 8
- When a handler returns `claimed: true`, dispatch stops for that event (no lower-priority handlers run)
- On `Err(...)` from `on_event()`, bus logs the error, wraps it in a `PluginError` event, and continues to next subscriber
- No backpressure: all events delivered synchronously and immediately
- `dispatch()` return value (`Vec<EventResult>`) is discarded by callers in most cases (exception: process.rs uses it to detect claimed/exec_error on plan.created)

**Priority ordering (lower number = runs first):**
- BUS_TRACER: 1 — traces events to file (configurable filters)
- JOB_MANAGER: 20 — handles job lifecycle and job.enqueue_requested events
- BACKUP_MANAGER: 30 — backs up before execution
- CAPABILITY_COLLECTOR: 35 — captures executor.capabilities for policy evaluator
- MKVTOOLNIX_EXECUTOR: 39 — claims plan.created for MKV (runs BEFORE ffmpeg); emits executor.capabilities from init()
- FFMPEG_EXECUTOR: 40 — claims plan.created; emits executor.capabilities from init()
- FFPROBE_INTROSPECTOR: 60 — subscribes to file.discovered, produces job.enqueue_requested via cascade
- DISCOVERY: 80 — no event subscriptions (direct API only)
- TOOL_DETECTOR: 90 — detects tools at init, emits tool.detected
- HEALTH_CHECKER: 95 — runs health checks at init
- STORAGE (sqlite-store): 100 — persists everything (highest priority = last to run)

**CRITICAL ORDERING NOTE:**
BUS_TRACER at priority 1 runs FIRST. But because claimed=true stops dispatch,
if mkvtoolnix (39) or ffmpeg (40) claim plan.created, bus-tracer still runs first
(priority 1) and sees the event BEFORE the executors — this is correct for tracing.

**CRITICAL ORDERING NOTE — Executor vs Backup:**
BackupManager at priority 30 handles PlanExecuting.
Executors at 39/40 handle PlanCreated.
In execute_single_plan(), PlanExecuting is dispatched BEFORE PlanCreated, so backup
happens before execution. This is correct by design but relies on CLI dispatch order,
not bus priority ordering within a single event type.

**Init-time event dispatch:**
- `Plugin::init()` returns `Result<Vec<Event>>` instead of `Result<()>`
- `Kernel::init_and_register()` dispatches all returned events through the bus AFTER the plugin is registered
- This means init-time events are seen by all already-registered subscribers
- Registration order in app.rs determines which plugins see init events from later-registered plugins

**PluginContext resource map:**
- `PluginContext` has a `resources: HashMap<TypeId, Arc<dyn Any>>` map
- Plugins retrieve shared resources via `ctx.resource::<T>()`
- Used to pass `Arc<JobQueue>` to job-manager at init time

**WASM interface version:** `@0.2.0` (world.wit, plugin.wit)
- Loader tries `voom:plugin/plugin@0.2.0` first, then falls back to bare `on-event` export
- No fallback to `@0.1.0`

**Shutdown:** On `Kernel::drop()`, shutdown() is called. Plugins are iterated in reverse priority order (highest number last = lowest priority last). Idempotent via `AtomicBool`.

**EventResult usage:**
- `kernel.dispatch()` returns `Vec<EventResult>`
- In most callers, this return value is DISCARDED (no `_results = kernel.dispatch(...)`)
- Exception: `execute_single_plan()` in process.rs uses it to check `claimed` and `execution_error`
- Plugin errors embedded in `produced_events` as PluginError events ARE cascaded and logged
  but since no subscriber handles them, they only appear in the event_log table

**sqlite-store catch-all:**
- `handles()` returns `true` for all event types (line 64-68 of sqlite-store/src/lib.rs)
- `on_event()` has a match on specific types, then `_ => {}` for the rest
- ALL events are logged to event_log table (best-effort, with auto-prune every 1000th insert)
- sqlite-store runs at priority 100 (LAST), so executors have already claimed/run before storage
