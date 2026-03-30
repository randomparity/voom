---
name: Plan lifecycle
description: Plan creation, mutation points, executor contract, serialization
type: project
---

## Plan lifecycle (current state)

1. DSL policy compiled → `CompiledPolicy` (lives in voom-dsl, not voom-domain)
2. `PolicyEvaluator::evaluate(&CompiledPolicy, &MediaFile) -> EvaluationResult`
   - Creates `Plan` locally, mutates fields during construction only, returns owned Vec<Plan>
   - `EvaluationOutcome` is private to evaluator.rs; `Failed` variant removed
   - `EvaluationResult.phase_outcomes` removed — internal state is no longer exposed
   - `apply_safeguards(&mut Plan, ...)` called at end of evaluate_phase() before Plan is returned — still under construction
3. `PhaseOrchestratorPlugin::orchestrate(Vec<Plan>) -> OrchestrationResult`
   - Consumes plans by value. No mutations to plans inside orchestrate().
   - `orchestrate()` is infallible.
4. CLI `process.rs` dispatches `PlanCreated` event with `plan.clone()`
   - Executors receive `&PlanCreatedEvent` containing an owned `Plan`
   - Executors call `&plan` (read-only) — no mutations in production code paths
5. Executors never mutate the plan they receive; they return `Vec<ActionResult>`

## Key invariant
Plans are never modified after being passed to executors. Executors read plan via `&Plan` only.

## executor_hint field
`Plan` has `executor_hint: Option<String>` with `#[serde(default)]`. Set by `apply_capability_hints` during construction (before PlanCreated dispatch). Executors don't currently consult it — it's informational/routing hint only.

## capability-validated evaluate
`PolicyEvaluatorPlugin::evaluate_with_capabilities()` wraps `evaluate_with_context()` + `apply_capability_hints(&mut result.plans, ...)`. The `&mut [Plan]` mutation is safe because it happens before Plans leave construction scope.

## Plan fields mutated during construction (eval phase only)
- `plan.policy_hash` — assigned after Plan::new()
- `plan.skip_reason` — assigned in skip/run_if/depends_on checks
- `plan.warnings` — pushed by error handlers and safeguard triggers
- `plan.safeguard_violations` — pushed by apply_safeguards()
- `plan.actions` — pushed and truncated during emit_* calls
- `plan.executor_hint` — set by apply_capability_hints(), before dispatch

All of these are normal construction-time mutations; none occur after the Plan is handed to a caller.

## Plan serialization context
`Plan` is the event payload in `PlanCreatedEvent` — it is cloned into the event. Executors receive `&PlanCreatedEvent` and access `&event.plan`. No mutation possible at the handler boundary.

## PhaseOrchestrator role
Library-only plugin — called directly by CLI, not registered with the kernel. Does not implement the Plugin trait. No capability advertised. Not part of the event bus dispatch graph.
