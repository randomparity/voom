---
name: Domain type immutability patterns
description: &mut self, pub fields, interior mutability findings across domain types
type: project
---

No `&mut self` methods exist on any core domain types (MediaFile, Track, Plan, PlannedAction, Event, Capability, etc.) in `crates/voom-domain/src/`. Zero interior mutability in domain types proper.

`InMemoryStore` (test_support.rs) uses `Mutex<HashMap>` — this is expected and acceptable for a test double.

All domain type fields are `pub` (not private with accessors) — this is an intentional design decision matching the `#[non_exhaustive]` pattern. Mutations are performed by building new structs or by local `mut` bindings before the value is returned.

`Plan` builder methods (`with_skip_reason`, `with_warning`, `with_action`) use `mut self` (consuming builder), not `&mut self`. This is the correct immutable-builder pattern.

Inside `evaluate_phase` (policy-evaluator/evaluator.rs), the local `plan` is mutated via direct field assignment before being returned. This is the construction phase — the `Plan` hasn't been handed to any caller yet, so it doesn't violate the contract. The pattern is: create locally, mutate locally during construction, return owned.

`apply_safeguards` and `apply_safeguard_for_track_type` take `&mut Plan` — but these are called only before the Plan leaves evaluate_phase(). The Plan is still under construction, not yet handed out.

`apply_capability_hints(plans: &mut [Plan], ...)` mutates Plan.warnings and Plan.executor_hint, but only during construction (inside `evaluate_with_capabilities`), before Plans are dispatched as events. Consistent with existing pattern.

Executor test helpers in ffmpeg-executor and mkvtoolnix-executor mutate Plan fields directly — all under `#[cfg(test)]`.

**Why:** The design principle is "mutations produce new values" but this is enforced at the API boundary (plans are passed as `&Plan` to executors), not at the field level.

## Infrastructure types with &mut self (acceptable)

`CapabilityMap::register(&mut self)` — construction-time operation only, used during plugin init bootstrap.

`CapabilityCollectorPlugin` holds `Mutex<CapabilityMap>` — infrastructure type, acceptable for cross-thread accumulation.

`BusTracerPlugin` holds `Option<Arc<Mutex<File>>>` — I/O writer, not a domain object.

`BackupManagerPlugin` holds `Mutex<HashMap<PathBuf, BackupRecord>>` — runtime state for in-flight backup records, infrastructure type.

`Plugin::init(&mut self, ...)` — the Plugin trait takes `&mut self` for init(). This is the one place plugins are allowed to mutate their own internal state (e.g., FfmpegExecutorPlugin stores probed_codecs, probed_formats after init). After init(), plugins are wrapped in `Arc<dyn Plugin>` and all subsequent event handling is via `&self`. This is a clean two-phase design: mutable init, then immutable operation.

`PhaseContext<'a>` in evaluator.rs holds `plan: &'a mut Plan` — local only, used to build the Plan before it is returned from evaluate_phase().

## Kernel-level &mut self (acceptable)

`Kernel::register_plugin(&mut self, ...)` — registration only, before any events are dispatched.
`PluginContext::register_resource(&mut self, ...)` — setup only, not called after init phase.
`SqlQuery::condition(&mut self, ...)` — builder pattern on a local query builder, not a domain type.
