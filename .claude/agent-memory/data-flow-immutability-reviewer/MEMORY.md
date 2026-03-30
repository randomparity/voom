# Agent Memory Index

## Domain model
- [Domain type immutability patterns](domain_immutability.md) — &mut self, pub fields, interior mutability findings across domain types
- [Plan lifecycle](plan_lifecycle.md) — Plan creation, mutation points, executor contract, serialization
- [PlanSummary redesign](plan_summary.md) — StoredPlan→PlanSummary change, typed actions, serialization gap

## Storage layer
- [SQLite store patterns](sqlite_store.md) — Transaction handling, INSERT vs UPDATE, history preservation

## Serialization
- [Serialization coverage](serialization.md) — serde(default), round-trip tests, PlanSummary missing Deserialize
