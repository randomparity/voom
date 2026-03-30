---
name: Serialization coverage
description: serde(default) usage, round-trip tests, PlanSummary missing Deserialize, ToolOutput/HttpResponse no serde
type: project
---

## Round-trip tests that exist
- `Plan`: JSON and msgpack round-trip tests in crates/voom-domain/src/plan.rs
- `Plan`: backward compat test (missing optional fields deserialize with defaults)
- `PlannedAction`, `ActionParams`, `OperationType`: tested via Plan round-trip
- `MediaFile`: JSON and msgpack round-trip tests in media.rs
- `Event` (various variants): JSON and msgpack round-trip tests in events.rs
- `BadFile`: JSON and msgpack round-trip tests in bad_file.rs
- `ProcessingStats`: JSON round-trip test in stats.rs
- `SafeguardViolation`: JSON round-trip test in safeguard.rs
- `ExecutorCapabilitiesEvent`, `CodecCapabilities`: both JSON+msgpack round-trip tests in events.rs
- `Job`: JSON round-trip test in job.rs

## serde(default) usage (plan.rs)
- `Plan.id`: `#[serde(default = "Uuid::new_v4")]` — generates fresh UUID on missing field
- `Plan.policy_hash`: `#[serde(default)]` — deserializes as None when absent
- `Plan.evaluated_at`: `#[serde(default = "epoch")]` — defaults to Unix epoch
- `Plan.safeguard_violations`: `#[serde(default)]` — deserializes as empty vec when absent
- `Plan.executor_hint`: `#[serde(default)]` — deserializes as None when absent

These are intentional backward-compat guards, not masking bugs.

## serde(default) usage (events.rs)
Several event fields use `#[serde(default)]` for optional fields (e.g. `claimed`, `execution_error`, `message`, `error_code`, `plugin_name`, `error_chain`, `keep_backups`, `hw_decoders`, `hw_encoders`). All are for backward compat on the event bus.

## PlanSummary: Serialize only, no Deserialize
`PlanSummary` derives `Serialize` but not `Deserialize`. It is a read model for API/templates. Not a WASM boundary type. No serialization round-trip tests needed for this type, but its absence should be noted if it ever needs to cross a plugin boundary.

## StoredPlan (sqlite-store private DTO)
`StoredPlan` is crate-private in plan_storage.rs. Stores `actions_json: String` and `warnings: Option<String>` as raw JSON blobs. `into_summary()` deserializes these. This means plan action data goes through two serialization layers (domain → JSON → DB → JSON → domain), but both use the same serde-derived types so fidelity is maintained.

Round-trip test exists: `plan_round_trip_preserves_diverse_action_params` in plan_storage.rs tests all ActionParams variants through the full SQLite round-trip.

## ToolOutput / HttpResponse: no serde at all
These types do not derive `Serialize`/`Deserialize` — they live only within the native host boundary (kernel ↔ plugin-sdk), not persisted or sent over msgpack. Correct.

## HealthStatusEvent
`HealthStatusEvent` derives Serialize/Deserialize. No dedicated round-trip test — only tested indirectly in the event bus. Minor gap.

## HealthCheckRecord
`HealthCheckRecord` derives Serialize/Deserialize. Lives in storage.rs. No dedicated round-trip test — only tested indirectly through SQLite insert/query.

## CapabilityMap: no serde
`CapabilityMap` derives Debug, Clone, Default. Does NOT derive Serialize/Deserialize. It's a transient aggregation, not persisted directly. ExecutorCapabilitiesEvent (which feeds CapabilityMap) IS serialized (JSON blob in plugin_data table). Correct.

## CompiledPolicy serialization (in voom-dsl)
`CompiledPolicy` lives in voom-dsl. Derives `Serialize`/`Deserialize`. Not exposed to WASM plugins directly (they get `Plan` structs via events). The move is correct since CompiledPolicy is a DSL IR type, not a domain type.

## No serde(skip) anywhere
Searched entire codebase: zero `#[serde(skip)]` attributes. No hidden state is silently dropped during serialization.
