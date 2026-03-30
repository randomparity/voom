---
name: VOOM Sprint 12+ Event Architecture Status
description: Post-Sprint-12 event bus state; updated for fix/multi-gh-issues branch which adds executor.capabilities, health.status, init-time event dispatch, and job lifecycle events
type: project
---

Policy-evaluator and phase-orchestrator were moved from kernel-registered (event bus) plugins to library-only plugins called directly by the CLI (process.rs: `orchestrate_plans`). As of Sprint 12 + desloppify branch, there are 7 kernel-registered plugins and 4 library-only plugins.

**Why:** The evaluator and orchestrator do not react to any events — they produce Plans from direct API calls. Registering them with the kernel gave them access to the event bus they never used, and added them as subscribers to all events with no matching handles().

**How to apply:** Do not recommend re-registering policy-evaluator or phase-orchestrator with the kernel unless they actually need to handle events.

The `EvaluationOutcome::Failed` variant was removed from the evaluator's internal enum. The enum now has only `Executed { modified: bool }` and `Skipped`. Evaluation cannot fail at the domain logic level (errors are captured in plans themselves).

**Why:** The variant was never reachable — evaluate() had no code path that produced it.

`execute_plans` was renamed to `dispatch_plan_events` in process.rs for naming clarity.

The `policy.evaluate` event from the spec table does NOT exist in the Event enum. It was a spec artifact; actual evaluation is done via direct API call.

**fix/multi-gh-issues branch additions:**
- `executor.capabilities` event: emitted by ffmpeg-executor and mkvtoolnix-executor from `init()`. Consumed by capability-collector (new in-process plugin, not persisted separately) and sqlite-store.
- `health.status` event: emitted by health-checker from `init()` and `run_checks()`. Only sqlite-store subscribes — no plugin reacts to failures.
- `Plugin::init()` signature changed from `Result<()>` to `Result<Vec<Event>>`. All kernel-registered plugins updated. WASM plugin default impl returns `Ok(vec![])`.
- `PluginContext` gains a typed resource map (`HashMap<TypeId, Arc<dyn Any>>`). Used to pass `Arc<JobQueue>` to ffprobe-introspector.
- `job.started/progress/completed` events now actually dispatched via `EventBusReporter` in process.rs (previously dead events).
- `tool.detected` events now properly dispatched via init_and_register (previously called detect_all() directly without bus dispatch).
- ffprobe-introspector is now kernel-registered (priority 60) and subscribes to `file.discovered`. It enqueues `JobType::Introspect` jobs but these are NOT consumed in scan/process modes (acknowledged: issue #36).
- `bus-tracer` new plugin at priority 1 for development debugging — logs events to file.
- WASM interface bumped from `@0.1.0` to `@0.2.0`.
- `capability-collector` is an in-process plugin (not in KNOWN_PLUGIN_NAMES for external listing) that aggregates executor capabilities for the policy evaluator.
