# File Lifecycle Tracking with Modification Provenance

**Date:** 2026-04-03
**Status:** Approved
**Branch:** fix/e2e-testing (to be moved to a feature branch at implementation)

## Problem

VOOM tracks files by path with UUID identity, but cannot distinguish between
files modified by voom and files modified externally. When a user deletes a file
and adds a new one at the same path, the old UUID is silently reused. There is
no way to answer "what has voom done to this file over time?" or "was this file
changed outside voom?"

## Goals

1. Track files from ingestion through their full lifecycle.
2. Voom modifications preserve file identity (same UUID).
3. External modifications at the same path create a new file identity.
4. File renames/moves detected by content hash preserve identity.
5. Crash recovery with configurable automatic restore.
6. Space savings and change attribution queryable per-file, per-phase.

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Approach | Option B: Modification Provenance | Domain concept (who changed what) over detection trick (hash comparison). Scales to future multi-tool workflows. |
| `processing_stats` | Keep separate | Stats = performance metrics, transitions = identity/provenance. Different concerns. |
| `file_history` | Replace with `file_transitions` | Transitions are a strict superset. |
| Soft-delete model | `active`/`missing` with `missing_since` timestamp | Configurable retention without a third status state. |
| Recovery config | `config.toml` only | Recovery is operational, not policy. |
| Transition granularity | Per phase | Matches existing per-phase architecture. |
| External change detection | At scan time | Scan is the natural reconciliation point; all downstream commands see correct identity immediately. |
| Move detection | Reactivate missing file matched by hash | Preserves transition chain through renames. |

## Schema Changes

### New table: `file_transitions`

Replaces `file_history`. Records every state change with full attribution.

```sql
CREATE TABLE file_transitions (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    path TEXT NOT NULL,           -- path at time of transition (preserved even if files.path is later NULLed)
    from_hash TEXT,               -- NULL for initial discovery
    to_hash TEXT NOT NULL,
    from_size INTEGER,            -- NULL for initial discovery
    to_size INTEGER NOT NULL,
    source TEXT NOT NULL,         -- 'discovery', 'voom', 'external', 'unknown'
    source_detail TEXT,           -- 'ffmpeg-executor:transcode', etc.
    plan_id TEXT,                 -- links to plans table when source='voom'
    created_at TEXT NOT NULL
);

CREATE INDEX idx_transitions_file ON file_transitions(file_id);
CREATE INDEX idx_transitions_source ON file_transitions(source);
```

Transition sources:
- `discovery` — file first seen, or reappeared after being missing.
- `voom` — voom modified the file. `source_detail` identifies the executor and
  phase; `plan_id` links to the plan.
- `external` — file at an existing path changed outside voom. Recorded on the
  OLD file_id before it is marked missing.
- `unknown` — crash recovery. `.vbak` exists with no completion record.

### Changes to `files` table

```sql
ALTER TABLE files ADD COLUMN expected_hash TEXT;
ALTER TABLE files ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
ALTER TABLE files ADD COLUMN missing_since TEXT;
```

- `expected_hash` — content hash voom expects to find on disk. Set after every
  voom modification and on initial discovery. Compared against on-disk hash
  during scan to detect external changes.
- `status` — `'active'` or `'missing'`.
- `missing_since` — timestamp when the file was first detected as missing.
  NULL when active. Pruning deletes records older than the retention window.

The `path` column constraint changes from `NOT NULL UNIQUE` to `UNIQUE`
(nullable). When a file is marked missing due to external replacement at the
same path, its `path` is set to NULL to release the uniqueness constraint for
the new record. Files marked missing due to disappearing from disk retain their
path (no new record needs the slot). The `file_transitions` table preserves the
historical path regardless.

### Dropped table: `file_history`

Replaced entirely by `file_transitions`. Table is dropped (pre-release, no
migration needed).

## Domain Type Changes

### New types in `voom-domain`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    Active,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionSource {
    Discovery,
    Voom,
    External,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTransition {
    pub id: Uuid,
    pub file_id: Uuid,
    pub path: PathBuf,
    pub from_hash: Option<String>,
    pub to_hash: String,
    pub from_size: Option<u64>,
    pub to_size: u64,
    pub source: TransitionSource,
    pub source_detail: Option<String>,
    pub plan_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}
```

### Changes to `MediaFile`

Two new fields:

```rust
pub expected_hash: Option<String>,
pub status: FileStatus,
```

### Removed types

- `FileHistoryEntry`
- `StoredHistoryRow`
- `FileHistoryStorage` trait

### New storage trait: `FileTransitionStorage`

```rust
pub trait FileTransitionStorage {
    fn record_transition(&self, transition: &FileTransition) -> Result<()>;
    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>>;
    fn transitions_by_source(&self, source: TransitionSource) -> Result<Vec<FileTransition>>;
}
```

### Changes to `FileStorage` trait

- New: `reconcile_discovered_files(&self, discovered: &[DiscoveredFile], scanned_dirs: &[PathBuf]) -> Result<ReconcileResult>` — batch identity reconciliation. `scanned_dirs` scopes which DB records are eligible for `missing` marking (only files whose paths fall under a scanned directory).
- `upsert_file` retained for post-introspection writes (`FileIntrospected` → persist full metadata with tracks). It no longer handles identity reconciliation — the file must already exist in the DB from reconciliation. It updates metadata, tracks, and `expected_hash` but does not change `id` or `status`.
- `delete_file` replaced by `mark_missing(&self, id: &Uuid) -> Result<()>`
- New: `purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64>`
- New: `reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()>`
- `list_files` / `count_files` filter by `status = 'active'` by default;
  `FileFilters` gains `include_missing: bool`

### New types for reconciliation

```rust
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
}

pub struct ReconcileResult {
    pub new_files: u32,
    pub unchanged: u32,
    pub moved: u32,
    pub external_changes: u32,
    pub missing: u32,
}
```

## Batch Reconciliation at Scan Time

The per-file event-driven upsert is replaced by a two-pass batch reconciliation.
This guarantees that missing files are identified before move detection runs.

### Pass 1: Mark missing

Compare the discovered set against the database, scoped to the directories that
were scanned. Only files whose paths fall under one of the `scanned_dirs` are
eligible — files in unscanned directories are left untouched. Any eligible path
in the DB that is currently `active` but NOT in the discovered set is marked
`status='missing', missing_since=now()`.

### Pass 2: Match new and changed files

For each discovered file:

1. **Path exists in DB, hash matches `expected_hash`** — unchanged. Preserve
   UUID, update size/metadata if needed. No transition recorded.
2. **Path exists in DB, hash differs from `expected_hash`** — external
   modification. Write an `external` transition on the old file_id. Clear the
   old record's `path` (set to NULL) and mark it `status='missing',
   missing_since=now()` — this releases the UNIQUE path constraint so the new
   record can claim it. Create a new record with new UUID at that path. Write
   a `discovery` transition on the new file_id.
3. **Path not in DB, hash matches a `missing` file's `expected_hash`** — move
   detected. Reactivate the missing record at the new path
   (`status='active', missing_since=NULL`, update path). Write a `discovery`
   transition with `source_detail='detected_move'`.
4. **Path not in DB, no hash match among missing files** — new file. Create
   record with new UUID. Write a `discovery` transition.

Both passes execute in a single database transaction.

### Impact on discovery plugin

The scanner still walks the filesystem and produces a list of discovered files.
The change is in how the results are consumed:

- CLI calls `reconcile_discovered_files()` directly on the storage trait,
  passing the full discovered set.
- `FileDiscovered` events are emitted only for files that need introspection
  (new, externally changed, moved). Unchanged files skip introspection.
- The sqlite-store plugin no longer subscribes to `FileDiscovered` for upsert
  purposes. It still receives `FileIntrospected` events to persist full
  metadata.

## Post-Execution Transition Recording

After each successful phase in `process_single_file_execute`:

1. Capture `from_hash` and `from_size` from `current_file` before execution.
2. Execute the plan (unchanged).
3. Re-introspect via `reintrospect_file` (unchanged — already produces new hash
   and size).
4. Record a `FileTransition`:
   - `source = Voom`
   - `source_detail = "{plugin_name}:{phase_name}"` (e.g.,
     `"mkvtoolnix-executor:normalize"`)
   - `plan_id` = the plan's UUID
5. Update `files.expected_hash` to the new content hash.

`source_detail` is assembled in `process.rs` from information already available
(executor plugin name from capability routing, phase name from the plan). The
executors themselves are not modified.

## Crash Recovery

### Detection

On `process` startup (before evaluating plans), scan backup locations for
orphaned `.vbak` files. Cross-reference against `event_log`: if a
`PlanExecuting` event exists for a path but no corresponding `PlanCompleted` or
`PlanFailed`, the backup is orphaned.

### Configuration

```toml
# ~/.config/voom/config.toml
[recovery]
mode = "always_recover"  # "always_recover" | "always_discard" | "prompt"
```

### Resolution

- **`always_recover`** — restore from `.vbak`. Write transition with
  `source='unknown', source_detail='crash_recovery:restored'`. Set
  `expected_hash` to the restored file's hash.
- **`always_discard`** — delete the `.vbak`, accept on-disk state. Write
  transition with `source='unknown', source_detail='crash_recovery:discarded'`.
  Set `expected_hash` to the current file's hash.
- **`prompt`** — list orphaned backups, ask per-file. Default for interactive
  sessions.

Recovery runs once at the start of `process`, not during `scan`.

## Missing File Pruning

### Configuration

```toml
# ~/.config/voom/config.toml
[pruning]
retention_days = 30
```

### When it runs

- As part of `voom db maintenance`.
- At the end of `voom scan` when missing files are detected.

### What it deletes

Files where `status = 'missing'` AND `missing_since` is older than
`retention_days`. Cascading deletes remove `tracks`, `plans`,
`processing_stats`, and `file_transitions` for the pruned file_id.

`library_snapshots` preserves aggregate historical data independent of
individual file records.

## Unchanged Components

These components are explicitly out of scope:

- **`processing_stats`** — kept as-is for performance metrics.
- **Backup manager** — no internal changes. Recovery check calls into it.
- **Executors** — no changes. Transitions recorded by `process.rs`.
- **Policy evaluator / phase orchestrator** — no changes.
- **DSL** — no changes.
- **Web UI / report / inspect commands** — deferred (GitHub issue).
- **WASM plugin interface (WIT types)** — deferred (GitHub issue).
- **`discovered_files` staging table** — still used for introspection staging.

## Deferred Work (GitHub Issues)

1. [#95](https://github.com/randomparity/voom/issues/95) — Web UI: file transition history view
2. [#96](https://github.com/randomparity/voom/issues/96) — Report command: space savings by provenance
3. [#97](https://github.com/randomparity/voom/issues/97) — WASM plugin types: expose `FileTransition` via WIT
4. [#98](https://github.com/randomparity/voom/issues/98) — Inspect command: show file transition history
5. [#99](https://github.com/randomparity/voom/issues/99) — Evaluate consolidating `processing_stats` into `file_transitions`
