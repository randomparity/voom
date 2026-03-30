---
name: Plugin Event Subscription Map
description: Which kernel-registered plugins subscribe to which events, and which events are emitted by the CLI vs plugins (updated for feat/address-cli-gaps-1 branch)
type: project
---

## Kernel-Registered Plugin Subscriptions

| Plugin | Priority | Handles (subscribes) | Emits via bus |
|--------|----------|----------------------|---------------|
| sqlite-store | 100 | ALL events (handles returns true always) — domain events get specific storage, everything goes to event_log | — |
| health-checker | 95 | (none) | health.status (from init()) |
| tool-detector | 90 | (none) | tool.detected (from init(), via detect_all()) |
| discovery | 80 | (none) | — (uses scan() direct API) |
| ffprobe-introspector | 60 | file.discovered | job.enqueue_requested (produced in EventResult.produced_events, cascaded) |
| capability-collector | 35 | executor.capabilities | — |
| mkvtoolnix-executor | 39 | plan.created | executor.capabilities (from init()) |
| ffmpeg-executor | 40 | plan.created | executor.capabilities (from init()) |
| backup-manager | 30 | plan.executing, plan.completed, plan.failed | — |
| job-manager | 20 | job.started, job.progress, job.completed, job.enqueue_requested | — |
| bus-tracer | 1 | configurable via filters (glob patterns) | — |

NOTE: capability-collector is registered via register_plugin (not init_and_register),
so it has no init() call. It is registered at priority 35, before executors (39/40),
so it is subscribed when executor init events fire.

NOTE: mkvtoolnix-executor at priority 39 and ffmpeg-executor at priority 40 both subscribe
to plan.created. mkvtoolnix runs BEFORE ffmpeg (lower number = earlier). Both use
EventResult.claimed=true to claim a plan; once one claims it, the other never sees it.

## CLI (process.rs) Event Emissions

- file.discovered — dispatched after direct scan
- file.introspected — dispatched after direct ffprobe call (via introspect.rs)
- file.introspection_failed — dispatched on ffprobe error
- plan.executing — dispatched before plan execution (triggers backup-manager)
- plan.created — dispatched to trigger executor plugins (claims plan)
- plan.completed — dispatched on executor success (triggers backup removal)
- plan.failed — dispatched on executor failure or unclaimed plan (triggers restore)
- plan.skipped — dispatched for skipped plans (after plan.created)
- job.started, job.progress, job.completed — dispatched via EventBusReporter

## CLI (scan.rs) Event Emissions

- file.discovered — dispatched for each found file
- file.introspected — dispatched after direct ffprobe call

## WASM Plugin Subscriptions (example-metadata)

- Subscribes to: file.introspected
- Emits: metadata.enriched (cascaded via produced_events in EventResult)
- metadata.enriched is consumed only by sqlite-store (handles returns true for all)

## Events with No Dedicated Subscribers (only sqlite-store via catch-all)

- plugin.error — emitted by bus internally when a handler errors or panics;
  only sqlite-store (via catch-all) logs it. No plugin reacts to plugin failures.
- health.status — only sqlite-store persists it; no plugin reacts to health failures.
- plan.skipped — only sqlite-store persists it; no plugin reacts to skips.
- file.introspection_failed — sqlite-store stores it as bad_file.

## Undocumented Events (not in spec table)

- file.introspection_failed — not in spec; emitted by CLI, subscribed by sqlite-store
- executor.capabilities — not in spec; emitted by executors at init, consumed by
  capability-collector and sqlite-store
- health.status — not in spec; emitted by health-checker at init
- plugin.error — not in spec; emitted by bus on handler failures
- plan.skipped — not in spec; emitted by CLI process.rs
- job.enqueue_requested — not in spec; emitted via produced_events cascade by
  ffprobe-introspector when it handles file.discovered

## Spec Events That Do NOT Exist

- policy.evaluate — this is a spec artifact; actual evaluation is a direct API call
  (voom_policy_evaluator::PolicyEvaluator::new().evaluate_with_capabilities)

## Persisted Job Queue Issue

- file.discovered dispatch causes ffprobe-introspector to produce JobEnqueueRequested
  events (cascaded). These are handled by job-manager and enqueued as JobType::Introspect
  in the SQLite-backed queue.
- In scan and process commands, these jobs are NEVER consumed from the queue (CLI drives
  introspection directly).
- Jobs accumulate in the jobs table with Pending status indefinitely.
- Acknowledged as future daemon-mode work (issue #36).
