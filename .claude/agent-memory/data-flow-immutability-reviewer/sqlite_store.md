---
name: SQLite store patterns
description: Transaction handling, INSERT vs UPDATE, history preservation, error handling changes
type: project
---

## Transaction handling
`upsert_file` uses rusqlite `conn.transaction()` for the archive+delete+upsert sequence. Atomic and correct.

## INSERT vs UPDATE / history preservation
Files are stored with `ON CONFLICT(path) DO UPDATE SET` (upsert by path). Before any upsert, the old file state is archived to `file_history` via INSERT. History is preserved at every re-introspection. First-time inserts do NOT create a history entry (correct — nothing to archive).

Plans are stored via INSERT only (no UPDATE). Plan status updates use a separate `update_plan_status()` call which does `UPDATE plans SET status = ?1, executed_at = ?2 WHERE id = ?3`. This is the only mutation path for stored Plans — status + timestamp only, never the action content.

## File ID preservation
When a file is re-introspected, `upsert_file` detects the existing DB row and preserves the original `id`. This prevents orphaning related records (plans, stats, history). The `Plan.file.id` may differ from the DB ID after re-introspection — `save_plan` resolves this by querying by path: `SELECT id FROM files WHERE path = ?1`.

## StorageErrorKind
`StorageErrorKind::ConnectionError` was removed. Pool/connection errors map to `StorageErrorKind::Other`. Uninitialized store returns `Err(VoomError::Plugin{...})`, not `Ok(None)`.

## Row mapper extraction
Row mapping functions live in `store/row_mappers.rs`. No behavior change from original inline row closures.

## Sprint 13 tables
`discovered_files`: staging table for file discovery pipeline. Uses upsert (INSERT ... ON CONFLICT DO UPDATE). No history preservation — re-discovery resets status to 'pending'. Intentional (staging table, not permanent record).

`health_checks`: append-only via INSERT. Pruned by `prune_health_checks(before: DateTime)`. `latest_health_checks()` uses MAX(checked_at) subquery join.

`DiscoveredStatus::from_str`: silent fallthrough to `Pending` for unknown values. This is a concern — unknown DB values are silently re-set instead of erroring.

`DiscoveredFile` type lives in `sqlite-store` crate only (not a domain type, not in `voom-domain::storage`). Correct — it's internal to the storage layer.

## chunked_delete
`chunked_delete(PruneTarget, ids)` uses `PruneTarget` enum to map variants to static (table, column) pairs, eliminating SQL injection from free-form string params.

## LIKE escaping
`escape_like()` helper escapes `%`, `_`, and `\` before using in LIKE patterns. All path prefix queries use this. Tests confirm escaping correctness.

## Concurrency model
`SqliteStore` uses r2d2 connection pool (default 8 connections). Each operation borrows a pooled connection. WAL mode is configured per connection. Concurrent pool access is tested. The store itself has no interior mutability beyond r2d2's internal pool management.

## Mutable reference exposure
Storage trait methods always take `&self` (not `&mut self`). Returned values are always owned (Vec<T>, Option<T>) — callers receive owned data, never mutable references to cached objects.
