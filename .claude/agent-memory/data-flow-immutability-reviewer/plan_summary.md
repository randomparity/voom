---
name: PlanSummary redesign
description: StoredPlan renamed to PlanSummary, typed actions Vec<PlannedAction> replaces actions_json String
type: project
---

## StoredPlan → PlanSummary (desloppify/code-health)

- Old `StoredPlan.actions_json: String` replaced by `PlanSummary.actions: Vec<PlannedAction>`
- Old `StoredPlan.warnings: Option<String>` replaced by `PlanSummary.warnings: Vec<String>`
- `PlanSummary` derives `Serialize` but NOT `Deserialize` — it's a read model for API/templates only
- `#[non_exhaustive]` added to PlanSummary

## Serialization gap
`PlanSummary` has no `Deserialize` impl. This is intentional (read model) but means:
- Cannot round-trip through msgpack or JSON
- Cannot be sent over the WASM boundary
- If ever needed for WIT types, requires adding `Deserialize`

## Internal DTO pattern in sqlite-store
`StoredPlan` (private to plan_storage.rs) maps database rows with raw JSON strings.
`into_summary()` deserializes `actions_json` → `Vec<PlannedAction>` and `warnings` JSON → `Vec<String>`.
Post-construction field assignment pattern used for optional fields (evaluated_at, executed_at, etc.) — this is acceptable since PlanSummary is `#[non_exhaustive]` and there is no builder API yet.

## PlanSummary.created_at always set to Utc::now() in constructor
The `PlanSummary::new()` constructor sets `created_at: Utc::now()`. The `into_summary()` method then overwrites `summary.created_at = self.created_at` from the DB row. Correct but fragile — the constructor default is misleading for the read-model use case.
