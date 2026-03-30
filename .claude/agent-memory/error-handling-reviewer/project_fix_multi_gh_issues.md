---
name: fix/multi-gh-issues branch context
description: Key error handling patterns introduced on the fix/multi-gh-issues branch (reviewed 2026-03-28)
type: project
---

New plugins added on this branch:
- `health-checker` — emits `HealthStatus` events, no error enum needed (infallible checks)
- `bus-tracer` — best-effort file I/O, init failures are warn-logged and degrade to no-op
- `discovered_file_storage.rs` — new staging table in sqlite-store
- `health_check_storage.rs` — new health check persistence in sqlite-store

Known issues identified on this branch:
- `DiscoveredStatus::from_str` silently defaults unknown values to `Pending` — mismatches map to wrong state without error or log
- `Uuid::parse_str(...).unwrap_or_default()` in `list_discovered_files` silently returns nil UUID on corrupt data
- `update_discovered_status` does not check `changes()` — a no-op update (path not found) returns `Ok(())` silently
- Health API: `all_passed` uses `Iterator::all` which returns `true` for empty — first request before any health checks reports "healthy"
- `serve.rs` periodic health loop discards `dispatch()` return value — storage failures go unobserved

**Why:** These were introduced as new code on this branch, not pre-existing debt.

**How to apply:** Flag these in future reviews of sqlite-store discovered_file_storage and the health checker loop in serve.rs.
