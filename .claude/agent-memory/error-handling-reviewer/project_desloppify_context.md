---
name: desloppify/code-health branch context
description: Key error handling changes being made on the desloppify/code-health branch and their implications
type: project
---

The desloppify/code-health branch (as of 2026-03-28) includes these error handling changes:

- `StorageErrorKind::ConnectionError` removed, `#[non_exhaustive]` added to both `StorageErrorKind` and `VoomError`
- `TrackType::FromStr` error type changed from `String` to `VoomError::Validation`
- `introspect_file` return type changed from `Result<_, String>` to `Result<_, VoomError>`
- `parse_job_payload` changed from `Result<_, String>` to `anyhow::Result`
- `orchestrate_plans` made infallible (no longer returns `Result`)
- `EvaluationOutcome::Failed` removed from evaluator
- Policy/phase orchestrator plugins de-registered from kernel (now library-only, called directly)
- `StoredPlan` renamed to `PlanSummary`; `actions_json: String` replaced with typed `actions: Vec<PlannedAction>`

**Why:** `ConnectionError` was already covered by `Other`. The `String` error types were inconsistent with the thiserror/anyhow strategy. Direct-call pattern for evaluator/orchestrator is architecturally cleaner.

**How to apply:** When suggesting further error handling improvements, note these are already addressed. Focus review energy on the two `map_err(|e| e.to_string())` calls that survive in `process_single_file` (lines 276, 288) — they exist because the worker pool's processor future has signature `Result<_, String>`, which is a WIT-boundary constraint that must remain.
