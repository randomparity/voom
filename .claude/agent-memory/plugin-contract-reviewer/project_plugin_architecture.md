---
name: VOOM plugin architecture
description: Two-tier plugin model, kernel-registered vs library-only plugins, capability/event contracts, and bootstrap sequence
type: project
---

## Kernel-registered native plugins (as of feat/address-cli-gaps-1)
11 kernel-registered + 3 library-only:

| Plugin | Capability | Handles Events | Priority | Registration |
|--------|-----------|----------------|----------|-------------|
| sqlite-store | Store{sqlite} | ALL events (handles() returns true unconditionally) | 100 | register_plugin (manual init) |
| health-checker | HealthCheck | (none — emits HealthStatus from init) | 95 | init_and_register |
| tool-detector | DetectTools | (none — emits ToolDetected from init) | 90 | init_and_register |
| discovery | Discover{file} | (none) | 80 | init_and_register |
| ffprobe-introspector | Introspect{mkv,...} | FileDiscovered → emits JobEnqueueRequested | 60 | init_and_register |
| capability-collector | (none) | ExecutorCapabilities | 35 | register_plugin |
| mkvtoolnix-executor | Execute{metadata+merge, mkv} | PlanCreated | 39 | init_and_register |
| ffmpeg-executor | Execute{...} | PlanCreated | 40 | init_and_register |
| backup-manager | Backup | PlanExecuting, PlanCompleted, PlanFailed | 30 | init_and_register |
| job-manager | ManageJobs | JobStarted, JobProgress, JobCompleted, JobEnqueueRequested | 20 | init_and_register |
| bus-tracer | (none) | filter-configured via glob patterns | 1 | init_and_register |

## Library-only plugins (NOT kernel-registered, no Plugin trait)
- `policy-evaluator` — called directly by `process.rs`
- `phase-orchestrator` — called directly by `process.rs`
- `web-server` — started by `voom serve` command

## Capability enum (voom-domain/src/capabilities.rs)
Discover, Introspect, Evaluate, Execute, Store, DetectTools, ManageJobs, ServeHttp, Plan, Backup, EnrichMetadata, Transcribe, Synthesize, HealthCheck

## Orphaned/uncovered capabilities
- `Capability::Evaluate` — WASM only
- `Capability::ServeHttp` — no native plugin claims it (web-server is library-only)
- `Capability::Plan` — library-only (phase-orchestrator)
- `Capability::Transcribe` — WASM only
- `Capability::Synthesize` — WASM only
- `Capability::EnrichMetadata` — WASM only

## Plugin init() contract
- Returns `Vec<Event>` (dispatched AFTER the plugin is registered on the bus)
- Executors (ffmpeg, mkvtoolnix) probe tool capabilities in init() and return ExecutorCapabilitiesEvent
- tool-detector returns ToolDetectedEvent(s) from init()
- health-checker returns HealthStatusEvent(s) from init()
- bus-tracer and discovery return empty vec from init()

## Capability routing
- CapabilityCollectorPlugin (priority 35) listens for ExecutorCapabilitiesEvent, builds CapabilityMap
- CapabilityCollectorPlugin is registered BEFORE executors (35 < 39 < 40) so it receives their init-time announcements
- process.rs calls collector.snapshot() after bootstrap to get CapabilityMap
- PolicyEvaluatorPlugin.evaluate_with_capabilities() adds warnings to plans + sets executor_hint
- FfmpegExecutorPlugin.can_handle() and MkvtoolnixExecutorPlugin.can_handle() consult probed data at dispatch time
- Executor priority ordering: mkvtoolnix (39) before ffmpeg (40) — MKV-specific ops go to mkvtoolnix first

## Lifecycle bootstrap (app.rs)
- sqlite-store initialized manually (store handle captured), registered via `register_plugin`
- All others registered via `init_and_register` (or `register_plugin` for capability-collector)
- `init()` failures propagate as errors via `with_context()` — kernel shuts down on any init failure
- `shutdown()` called via `Kernel::Drop` — called for all bus subscribers in reverse priority order
- Double-shutdown is safe via AtomicBool guard in Kernel::shutdown()
- PluginContext has a typed resource map (register_resource/resource<T>) for sharing handles (JobQueue, etc.)

## Event bus (crates/voom-kernel/src/bus.rs)
- Priority-ordered synchronous dispatch (lower value = dispatched first)
- MAX_CASCADE_DEPTH = 8 prevents infinite event loops
- Panics in on_event() are caught via catch_unwind, produce PluginError events — bus continues
- Claimed events (claimed: true in EventResult) stop dispatch to lower-priority handlers
- All subscribers get shutdown() called in reverse-priority order on Kernel Drop

## WIT boundary (crates/voom-wit/src/convert.rs)
- event_to_wasm / event_from_wasm use rmp_serde (MessagePack) — transparent for new event variants
- capability_to_wit / capability_from_wit: all 14 Capability variants handled explicitly + safe fallback
- HealthCheck, ExecutorCapabilities, JobEnqueueRequested all have test coverage for roundtrip

**Why:** Architectural context for auditing Plugin trait contracts.
**How to apply:** When reviewing new plugins, check they follow the two-tier model and the capability/event contract described here.
