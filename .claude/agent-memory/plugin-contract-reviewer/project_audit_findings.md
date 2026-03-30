---
name: Branch audit findings
description: Plugin contract audit findings per branch, most recent first
type: project
---

## Audit: feat/address-cli-gaps-1 vs main (2026-03-29)

### Branch context
This branch adds CLI gap features (config get/set, db stats, plan --plan-only, policy diff). No plugin architecture changes were made in this branch. The state of all 11 kernel-registered plugins is inherited from the fix/multi-gh-issues branch.

### Key finding from prior audit now FIXED
- **capability_to_wit unreachable arm**: The previously-CRITICAL bug at `crates/voom-wit/src/convert.rs` is fixed. The `_ => unreachable!()` arm is now `other => other.kind().to_string()`, covering all Capability variants safely. HealthCheck, EnrichMetadata, etc. now have explicit arms AND a safe fallback.

### New/updated findings for this branch

#### INFO — No new plugin contract issues found
All plugin trait implementations are clean on this branch. No new plugins were added.

### Comprehensive Plugin-Capability Matrix (current state)
| Plugin | Version | Declared Capabilities | Handled Events | Registration |
|--------|---------|----------------------|----------------|-------------|
| sqlite-store | CARGO_PKG_VERSION | Store{sqlite} | ALL (returns true unconditionally) | register_plugin (manual init) |
| bus-tracer | CARGO_PKG_VERSION | (none) | filter-configured via glob patterns | init_and_register (priority 1) |
| health-checker | CARGO_PKG_VERSION | HealthCheck | (none — init emits HealthStatus events) | init_and_register (priority 95) |
| tool-detector | CARGO_PKG_VERSION | DetectTools | (none — init emits ToolDetected events) | init_and_register (priority 90) |
| discovery | CARGO_PKG_VERSION | Discover{file} | (none) | init_and_register (priority 80) |
| ffprobe-introspector | CARGO_PKG_VERSION | Introspect{mkv,mp4,avi,wmv,flv,mov,ts} | FileDiscovered → emits JobEnqueueRequested | init_and_register (priority 60) |
| capability-collector | 0.1.0 hardcoded | (none) | ExecutorCapabilities | register_plugin (priority 35) |
| mkvtoolnix-executor | CARGO_PKG_VERSION | Execute{metadata+MERGE_OPS, mkv} | PlanCreated | init_and_register (priority 39) |
| ffmpeg-executor | CARGO_PKG_VERSION | Execute{transcode+..., all-formats} | PlanCreated | init_and_register (priority 40) |
| backup-manager | CARGO_PKG_VERSION | Backup | PlanExecuting, PlanCompleted, PlanFailed | init_and_register (priority 30) |
| job-manager | CARGO_PKG_VERSION | ManageJobs | JobStarted, JobProgress, JobCompleted, JobEnqueueRequested | init_and_register (priority 20) |

### Library-only plugins (NOT kernel-registered, no Plugin trait)
- policy-evaluator — called directly by process.rs
- phase-orchestrator — called directly by process.rs
- web-server — started by `voom serve` command

### Capability coverage
- Discover: discovery
- Introspect: ffprobe-introspector
- Store: sqlite-store
- Execute: ffmpeg-executor + mkvtoolnix-executor (intentional overlap, different specializations by container type and operation)
- DetectTools: tool-detector
- ManageJobs: job-manager
- Backup: backup-manager
- HealthCheck: health-checker
- Evaluate: WASM only (uncovered natively)
- ServeHttp: uncovered (web-server is not a Plugin)
- Plan: uncovered (phase-orchestrator is library-only)
- Transcribe: WASM only (uncovered natively)
- Synthesize: WASM only (uncovered natively)
- EnrichMetadata: WASM only (uncovered natively)

### Persistent open issues (not yet fixed)
1. **capability-collector version is hardcoded "0.1.0"** — all other plugins use env!("CARGO_PKG_VERSION"). Minor inconsistency.
2. **capability-collector registered via register_plugin not init_and_register** — bypasses lifecycle path. No init()/shutdown() so benign, but plugin won't appear in shutdown sequence.
3. **WASM plugins bypass init_and_register** — loaded via WasmPluginLoader then registered via register_plugin. init() is called inside the loader, not via kernel lifecycle. Shutdown IS called (all bus subscribers get shutdown via Drop), so lifecycle is safe but inconsistent.
4. **sqlite-store handles() returns true for ALL events including unknown future ones** — intentional (for event log table), but means any future event variant with no explicit match arm falls through to the `_ => {}` arm silently. Documented behavior but worth noting.
5. **BackupManagerPlugin has no shutdown() implementation** — holds Mutex<HashMap<PathBuf, BackupRecord>>. Any active backups at shutdown time are leaked (backup files remain on disk). No explicit cleanup. This is partially by design (crash recovery) but there is no documentation of this behavior.

**Why:** Historical reference for future audits — tracks what changed, what was fixed, and what persistent issues remain.
**How to apply:** When reviewing new plugins or changes, compare against this matrix to identify regressions or new gaps.

---

## Audit: fix/multi-gh-issues vs main (2026-03-28)

### Changes audited
1. Plugin trait: 4 new optional metadata methods (description, author, license, homepage) with default empty-string impls
2. New plugins: health-checker, bus-tracer
3. ffprobe-introspector promoted from library-only to kernel-registered Plugin
4. Executor init() probing: ffmpeg probes codecs/formats/hw_accels; mkvtoolnix probes availability
5. ExecutorCapabilitiesEvent + HealthStatusEvent added to Event enum
6. CapabilityMap + CapabilityCollectorPlugin for runtime routing data
7. validate_against_capabilities() in policy-evaluator; evaluate_with_capabilities() wrapper
8. Plan.executor_hint field for single-executor routing hint
9. sqlite-store: new discovered_files and health_checks tables + FILE_DISCOVERED and HEALTH_STATUS handling
10. tool-detector init() now returns ToolDetected events instead of void
11. init() return type changed: () → Vec<Event> across all plugins
12. WIT interface bumped to 0.2.0
13. EventBusReporter in process.rs: dispatches JobStarted/Progress/Completed through bus

### Key findings (now historical)

#### CRITICAL — FIXED in feat/address-cli-gaps-1
- **capability_to_wit unreachable arm**: `crates/voom-wit/src/convert.rs:173` — `_ => unreachable!()` will panic at runtime when a WASM plugin declares `HealthCheck`. Fixed: now `other => other.kind().to_string()`.

#### WARNING
- **HealthStatus event not in WIT converter**: `crates/voom-wit/src/convert.rs` — `event_to_wasm`/`event_from_wasm` use serde msgpack which will handle the new variants transparently, but there is no explicit WASM handling/tests for HealthStatus and ExecutorCapabilities events being passed to WASM plugins.
- **capability-collector uses register_plugin not init_and_register**: `crates/voom-cli/src/app.rs:178` — CapabilityCollectorPlugin bypasses the lifecycle path. It has no init() or shutdown() so this is benign, but it's inconsistent.
- **bus-tracer: event_summary has wildcard fallback** `plugins/bus-tracer/src/lib.rs` — `_ => String::new()` means new event variants produce empty summaries in traces. Low impact but worth noting.
