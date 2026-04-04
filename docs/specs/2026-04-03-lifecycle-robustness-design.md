# Lifecycle Tracking Robustness Fixes

> Single spec covering four hardening fixes for the file lifecycle tracking feature.

## Problem Statement

The lifecycle tracking implementation is architecturally sound but has four boundary-level gaps that will cause real failures:

1. **No concurrency guard** — two VOOM processes can stomp on each other's SQLite writes (cron + manual use)
2. **Event log pruning kills crash recovery** — the 10K-row auto-prune can delete `plan.executing` events that crash recovery needs
3. **String-based path comparison** — macOS case-insensitivity and Unicode NFD decomposition create phantom duplicates
4. **Crash recovery parses unstructured strings** — event summary format `"path=/foo phase=bar"` is an implicit contract with no type safety

## Fix 1: Process Lock

### Design

File-based exclusive lock using `flock()` on `<data_dir>/voom.lock`.

**Scope:** Acquired at CLI startup for commands that write to SQLite:
- **Locked:** `scan`, `process`, `db prune`, `jobs cancel`, `jobs retry`, `jobs clear`, `serve`
- **Not locked:** `status`, `report`, `history`, `inspect`, `policy`, `config`, `doctor`, `jobs list`, `jobs status`, `plugin`

Note: `serve` acquires the lock because it runs background health-check writes and exposes web API endpoints that mutate file status (e.g., `DELETE /api/files/:id`). Since `serve` is long-running, the lock is held for the lifetime of the server — this intentionally prevents concurrent `scan` or `process` while the server is up. Users who want to scan while serving must stop the server first (or use `--force`).

**Behavior:**
- Lock attempted non-blocking (`LOCK_EX | LOCK_NB`)
- On failure: print `"Another voom process is running. Use --force to override."` and exit 1
- `--force` flag on mutating commands skips the lock check (stuck-lock recovery)
- Lock released automatically on process exit (OS-level flock semantics — no stale locks after crash)
- No PID file needed

**Location:** New module `crates/voom-cli/src/lock.rs`, called from `main.rs` before command dispatch.

**Out of scope:** NFS/network filesystem protection. SQLite itself has documented limitations there, and flock is unreliable over NFS.

## Fix 2: Path Normalization

### Design

A `normalize_path()` function applying two transformations at the system boundary:

1. **`std::fs::canonicalize()`** — resolves symlinks, `..` components, and macOS `/var` → `/private/var`
2. **Unicode NFC normalization** — recomposes macOS NFD-decomposed filenames so `é` (U+00E9) and `e` + `◌́` (U+0065 U+0301) compare equal

**Application points (boundary only):**
- `plugins/discovery/src/scanner.rs` — normalize paths as they come off `walkdir`, before hashing or emitting events
- `crates/voom-cli/src/recovery.rs` — normalize the inferred original path from backup filenames

All downstream code (reconciliation, transition recording, event summaries) automatically operates on normalized paths. No changes inside sqlite-store reconciliation logic.

**Fallback:** If `canonicalize()` fails (file deleted between walk and normalization), use the raw path. Matches existing scanner error-handling pattern.

**Dependency:** `unicode-normalization` crate (well-maintained, used by `url` and `idna`, zero unsafe code).

**Not included:** Case-folding. Would change displayed paths on case-sensitive Linux filesystems.

## Fix 3: Dedicated Recovery Table

### Design

A new `pending_operations` table that crash recovery queries instead of parsing event log summaries. Fixes both the pruning problem (#2) and the unstructured parsing problem (#4).

**Event pipeline change:** Add `plan_id: Uuid` field to `PlanExecutingEvent`. The plan UUID is already available at the dispatch site (`execute_single_plan` in `process.rs` has the `Plan` struct with its `id` field). This is currently the only event in the executing/completed/failed trio that lacks a plan_id.

**Schema:**
```sql
CREATE TABLE IF NOT EXISTS pending_operations (
    id TEXT PRIMARY KEY,          -- plan UUID (from PlanExecutingEvent.plan_id)
    file_path TEXT NOT NULL,      -- normalized path of file being processed
    phase_name TEXT NOT NULL,     -- phase that's executing
    started_at TEXT NOT NULL      -- when execution began
);
```

Note: no `backup_path` column. The backup-manager creates `.vbak` files *in response to* `PlanExecuting`, so the backup path isn't known when the row is inserted. Recovery continues to find backups by scanning `.voom-backup/` directories (which it already does). The table's job is to know which files have in-flight operations, not to track backup locations.

**Lifecycle:**
1. **Insert** when `PlanExecuting` event is handled by sqlite-store plugin (using the new `plan_id` field)
2. **Delete** when `PlanCompleted` or `PlanFailed` event is handled (these already carry `plan_id`)
3. On clean execution, a row exists for milliseconds. On crash, it persists.

**Crash recovery changes to `recovery.rs`:**
- Query `pending_operations` instead of event log
- Any row present at startup = orphan (crashed between executing and completed)
- `file_path` and `phase_name` are structured columns — no summary string parsing
- Backup discovery remains filesystem-based (scan `.voom-backup/` dirs), but now cross-referenced against `pending_operations.file_path` for confirmation
- After recovery action (restore or discard), delete the row

**Event log unchanged:** Keeps 10K auto-pruning, keeps logging all events. Stays as a general audit log. Crash recovery no longer depends on it.

**Migration:** Added to `ensure_schema()` as a new `CREATE TABLE IF NOT EXISTS`. No data migration. Existing orphaned `.vbak` files still detected by the filesystem scan fallback.

## Fix 4: Testing Strategy

### Unit Tests

- **`lock.rs`:** Lock acquisition, contention (spawn child process, verify failure), `--force` bypass
- **`normalize_path()`:** Canonicalize fallback on missing file, NFC normalization of decomposed Unicode, no-op on already-normalized paths
- **`pending_operations` storage:** Insert, delete, query, verify persistence across connections

### Functional Tests (in `test_lifecycle_advanced`)

- **Lock contention:** Spawn background `voom scan` holding the lock, try second `voom scan`, assert exit 1 with error message
- **Path normalization round-trip:** Create file with decomposed Unicode name, scan, move it, rescan — verify move detection (same UUID) instead of duplicate creation
- **Recovery table orphan detection:** Insert `pending_operations` row directly, run `voom process`, verify orphan detected and recovered without event log queries
- **Recovery survives pruning:** Insert `pending_operations` row, generate >10K events to trigger pruning, run recovery — verify orphan still found

### Existing Test Updates

The 22 existing lifecycle tests should pass unchanged for the lock and path normalization changes. The three crash recovery tests (`crash_recovery_always_recover`, `crash_recovery_always_discard`, `normal_backup_not_treated_as_orphan`) need updating to insert into `pending_operations` instead of (or in addition to) `event_log`.

## Files Modified

| File | Change |
|------|--------|
| `crates/voom-cli/src/lock.rs` | New module: flock-based process lock |
| `crates/voom-cli/src/main.rs` | Acquire lock before mutating command dispatch |
| `plugins/discovery/src/scanner.rs` | Apply `normalize_path()` to discovered paths |
| `plugins/discovery/src/lib.rs` or `scanner.rs` | New `normalize_path()` function |
| `crates/voom-domain/src/events.rs` | Add `plan_id: Uuid` to `PlanExecutingEvent` |
| `crates/voom-cli/src/commands/process.rs` | Pass `plan.id` when constructing `PlanExecutingEvent` |
| `plugins/sqlite-store/src/schema.rs` | Add `pending_operations` CREATE TABLE |
| `plugins/sqlite-store/src/lib.rs` | Insert/delete `pending_operations` on plan events |
| `plugins/sqlite-store/src/store/` | New storage functions for `pending_operations` |
| `crates/voom-cli/src/recovery.rs` | Rewrite to query `pending_operations`, normalize paths |
| `crates/voom-cli/tests/functional_tests.rs` | New tests + update 3 existing crash recovery tests |
| `Cargo.toml` (discovery or domain) | Add `unicode-normalization` dependency |

## Dependencies Between Fixes

```
Fix 1 (Lock) ──── independent, can be implemented first
Fix 2 (Paths) ─── independent, can be implemented in parallel
Fix 3 (Recovery Table) ─── depends on Fix 2 (paths stored normalized)
Fix 4 (Tests) ─── depends on all three above
```

Fix 1 and Fix 2 have no interaction. Fix 3 depends on Fix 2 because paths stored in `pending_operations.file_path` should be normalized. Tests come last.
