# File Lifecycle Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement modification provenance tracking so voom can distinguish its own file changes from external modifications, detect renames/moves by content hash, and maintain a full transition history per file.

**Architecture:** New `file_transitions` table replaces `file_history`. Files gain `expected_hash`, `status`, and `missing_since` columns. Discovery switches from per-file event-driven upserts to batch two-pass reconciliation (mark missing, then match). Post-execution transition recording in `process.rs`. Crash recovery and time-based pruning configured via `config.toml`.

**Tech Stack:** Rust, rusqlite, serde, chrono, uuid, xxhash (existing deps only)

**Spec:** `docs/specs/2026-04-03-file-lifecycle-tracking-design.md`

---

### Task 1: Add domain types (`FileStatus`, `TransitionSource`, `FileTransition`, `DiscoveredFile`, `ReconcileResult`)

**Files:**
- Create: `crates/voom-domain/src/transition.rs`
- Modify: `crates/voom-domain/src/lib.rs`
- Modify: `crates/voom-domain/src/media.rs:11-23`
- Test: inline `#[cfg(test)]` in `transition.rs`

- [ ] **Step 1: Write tests for new domain types**

Add `crates/voom-domain/src/transition.rs`:

```rust
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Whether a file is actively tracked or has disappeared from disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
    Active,
    Missing,
}

impl FileStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Missing => "missing",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "missing" => Self::Missing,
            _ => Self::Active,
        }
    }
}

/// What caused a file state transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionSource {
    Discovery,
    Voom,
    External,
    Unknown,
}

impl TransitionSource {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Voom => "voom",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "discovery" => Self::Discovery,
            "voom" => Self::Voom,
            "external" => Self::External,
            "unknown" => Self::Unknown,
            _ => Self::Unknown,
        }
    }
}

/// A recorded state change for a tracked file.
#[non_exhaustive]
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

impl FileTransition {
    /// Create a new transition with a fresh UUID and current timestamp.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        file_id: Uuid,
        path: PathBuf,
        from_hash: Option<String>,
        to_hash: String,
        from_size: Option<u64>,
        to_size: u64,
        source: TransitionSource,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            file_id,
            path,
            from_hash,
            to_hash,
            from_size,
            to_size,
            source,
            source_detail: None,
            plan_id: None,
            created_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.source_detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_plan_id(mut self, plan_id: Uuid) -> Self {
        self.plan_id = Some(plan_id);
        self
    }
}

/// A file discovered during filesystem scanning, before reconciliation.
#[derive(Debug, Clone)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: String,
}

/// Summary of what changed during batch reconciliation.
#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    pub new_files: u32,
    pub unchanged: u32,
    pub moved: u32,
    pub external_changes: u32,
    pub missing: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_status_roundtrip() {
        assert_eq!(
            FileStatus::from_str_lossy(FileStatus::Active.as_str()),
            FileStatus::Active
        );
        assert_eq!(
            FileStatus::from_str_lossy(FileStatus::Missing.as_str()),
            FileStatus::Missing
        );
        assert_eq!(
            FileStatus::from_str_lossy("garbage"),
            FileStatus::Active
        );
    }

    #[test]
    fn transition_source_roundtrip() {
        for source in [
            TransitionSource::Discovery,
            TransitionSource::Voom,
            TransitionSource::External,
            TransitionSource::Unknown,
        ] {
            assert_eq!(
                TransitionSource::from_str_lossy(source.as_str()),
                source
            );
        }
        assert_eq!(
            TransitionSource::from_str_lossy("garbage"),
            TransitionSource::Unknown
        );
    }

    #[test]
    fn file_transition_builder() {
        let file_id = Uuid::new_v4();
        let plan_id = Uuid::new_v4();
        let t = FileTransition::new(
            file_id,
            PathBuf::from("/test.mkv"),
            None,
            "abc123".into(),
            None,
            1000,
            TransitionSource::Discovery,
        )
        .with_detail("detected_move")
        .with_plan_id(plan_id);

        assert_eq!(t.file_id, file_id);
        assert_eq!(t.source_detail.as_deref(), Some("detected_move"));
        assert_eq!(t.plan_id, Some(plan_id));
        assert!(t.from_hash.is_none());
        assert_eq!(t.to_hash, "abc123");
    }

    #[test]
    fn reconcile_result_default_is_zero() {
        let r = ReconcileResult::default();
        assert_eq!(r.new_files, 0);
        assert_eq!(r.unchanged, 0);
        assert_eq!(r.moved, 0);
        assert_eq!(r.external_changes, 0);
        assert_eq!(r.missing, 0);
    }
}
```

- [ ] **Step 2: Run test to verify it compiles and passes**

Run: `cargo test -p voom-domain -- transition`
Expected: All 4 tests pass.

- [ ] **Step 3: Add `FileStatus` and `expected_hash` to `MediaFile`**

In `crates/voom-domain/src/media.rs`, add field after `content_hash` (line 15):

```rust
    pub expected_hash: Option<String>,
    pub status: crate::transition::FileStatus,
```

Update `MediaFile::new()` (around line 27) to initialize:

```rust
            expected_hash: None,
            status: crate::transition::FileStatus::Active,
```

- [ ] **Step 4: Wire up module and re-exports in `lib.rs`**

In `crates/voom-domain/src/lib.rs`, add after `pub mod temp_file;` (line 15):

```rust
pub mod transition;
```

Add to the re-exports section:

```rust
pub use transition::{
    DiscoveredFile, FileStatus, FileTransition, ReconcileResult, TransitionSource,
};
```

- [ ] **Step 5: Fix compilation errors from new `MediaFile` fields**

The new fields on `MediaFile` will cause compilation errors in:
- `crates/voom-domain/src/test_support.rs` — `test_media_file()` helper
- `plugins/ffprobe-introspector/` — wherever `MediaFile` is constructed
- `crates/voom-cli/src/commands/process.rs` — `reintrospect_file`
- `plugins/sqlite-store/src/store/row_mappers.rs` — `FileRow::to_media_file()`

For each: add `expected_hash: None, status: FileStatus::Active` to the struct initialization. Since `MediaFile` is `#[non_exhaustive]`, constructors go through `new()` or builders — update those.

Run: `cargo build 2>&1 | head -50` to find all sites, fix each.

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: All existing tests pass. New transition tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-domain/src/transition.rs crates/voom-domain/src/lib.rs crates/voom-domain/src/media.rs
git add -u  # pick up compilation fixes
git commit -m "feat: add FileTransition domain types and FileStatus to MediaFile"
```

---

### Task 2: Add storage traits (`FileTransitionStorage`, updated `FileStorage`)

**Files:**
- Modify: `crates/voom-domain/src/storage.rs:36-100`
- Modify: `crates/voom-domain/src/lib.rs:41-46`
- Modify: `crates/voom-domain/src/test_support.rs:96-186`

- [ ] **Step 1: Replace `FileHistoryStorage` with `FileTransitionStorage`**

In `crates/voom-domain/src/storage.rs`, remove the `FileHistoryStorage` trait (lines 94-100) and add:

```rust
/// File transition history operations.
///
/// # Errors
/// Methods return `VoomError::Storage` on database failures.
pub trait FileTransitionStorage: Send + Sync {
    fn record_transition(&self, transition: &FileTransition) -> Result<()>;
    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>>;
    fn transitions_by_source(
        &self,
        source: TransitionSource,
    ) -> Result<Vec<FileTransition>>;
}
```

Add the necessary imports at the top of `storage.rs`:

```rust
use crate::transition::{
    DiscoveredFile, FileTransition, ReconcileResult, TransitionSource,
};
```

- [ ] **Step 2: Update `FileStorage` trait**

Replace `delete_file` with `mark_missing` and add new methods. The updated trait (replacing lines 51-59):

```rust
pub trait FileStorage: Send + Sync {
    fn upsert_file(&self, file: &MediaFile) -> Result<()>;
    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>>;
    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>>;
    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>>;
    fn count_files(&self, filters: &FileFilters) -> Result<u64>;
    fn mark_missing(&self, id: &Uuid) -> Result<()>;
    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()>;
    fn purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64>;
    fn reconcile_discovered_files(
        &self,
        discovered: &[DiscoveredFile],
        scanned_dirs: &[PathBuf],
    ) -> Result<ReconcileResult>;
}
```

Add `use std::path::PathBuf;` to the imports if not already present.

- [ ] **Step 3: Add `include_missing` to `FileFilters`**

In `FileFilters` struct (line 36), add:

```rust
    pub include_missing: bool,
```

Since `FileFilters` derives `Default`, `include_missing` defaults to `false` (only active files shown).

- [ ] **Step 4: Remove `FileHistoryEntry`, `StoredHistoryRow` types**

Delete lines 456-540 from `storage.rs` (the `StoredHistoryRow` and `FileHistoryEntry` structs and their impls).

- [ ] **Step 5: Update `lib.rs` re-exports**

In `crates/voom-domain/src/lib.rs`, replace `FileHistoryStorage` with the new exports in the storage re-export block (lines 41-46):

```rust
pub use storage::{
    BadFileFilters, BadFileStorage, EventLogFilters, EventLogRecord, EventLogStorage, FileFilters,
    FileStorage, FileTransitionStorage, HealthCheckFilters, HealthCheckRecord, HealthCheckStorage,
    JobFilters, JobStorage, MaintenanceStorage, PlanStorage, PlanSummary, PluginDataStorage,
    SnapshotStorage, StatsStorage, StorageTrait,
};
```

- [ ] **Step 6: Update `InMemoryStore` in `test_support.rs`**

Replace the `FileStorage` impl (lines 136-186) to use `mark_missing` instead of `delete_file`, and add stub implementations for new methods:

```rust
impl FileStorage for InMemoryStore {
    fn upsert_file(&self, file: &MediaFile) -> Result<()> {
        self.files.lock().unwrap().insert(file.id, file.clone());
        Ok(())
    }

    fn file(&self, id: &Uuid) -> Result<Option<MediaFile>> {
        Ok(self.files.lock().unwrap().get(id).cloned())
    }

    fn file_by_path(&self, path: &Path) -> Result<Option<MediaFile>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .values()
            .find(|f| f.path == path)
            .cloned())
    }

    fn list_files(&self, filters: &FileFilters) -> Result<Vec<MediaFile>> {
        let files = self.files.lock().unwrap();
        let mut result: Vec<MediaFile> = files
            .values()
            .filter(|f| {
                if !filters.include_missing
                    && f.status == crate::transition::FileStatus::Missing
                {
                    return false;
                }
                matches_filter(f, filters)
            })
            .cloned()
            .collect();
        result.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(offset) = filters.offset {
            result = result.into_iter().skip(offset as usize).collect();
        }
        if let Some(limit) = filters.limit {
            result.truncate(limit as usize);
        }
        Ok(result)
    }

    fn count_files(&self, filters: &FileFilters) -> Result<u64> {
        let files = self.files.lock().unwrap();
        let count = files
            .values()
            .filter(|f| {
                if !filters.include_missing
                    && f.status == crate::transition::FileStatus::Missing
                {
                    return false;
                }
                matches_filter(f, filters)
            })
            .count();
        Ok(count as u64)
    }

    fn mark_missing(&self, id: &Uuid) -> Result<()> {
        if let Some(file) = self.files.lock().unwrap().get_mut(id) {
            file.status = crate::transition::FileStatus::Missing;
        }
        Ok(())
    }

    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        if let Some(file) = self.files.lock().unwrap().get_mut(id) {
            file.status = crate::transition::FileStatus::Active;
            file.path = new_path.to_path_buf();
        }
        Ok(())
    }

    fn purge_missing(&self, _older_than: DateTime<Utc>) -> Result<u64> {
        let mut files = self.files.lock().unwrap();
        let before = files.len();
        files.retain(|_, f| f.status != crate::transition::FileStatus::Missing);
        Ok((before - files.len()) as u64)
    }

    fn reconcile_discovered_files(
        &self,
        _discovered: &[crate::transition::DiscoveredFile],
        _scanned_dirs: &[PathBuf],
    ) -> Result<crate::transition::ReconcileResult> {
        Ok(crate::transition::ReconcileResult::default())
    }
}
```

Also add a stub `FileTransitionStorage` impl for `InMemoryStore`:

```rust
impl FileTransitionStorage for InMemoryStore {
    fn record_transition(
        &self,
        _transition: &crate::transition::FileTransition,
    ) -> Result<()> {
        Ok(())
    }

    fn transitions_for_file(
        &self,
        _file_id: &Uuid,
    ) -> Result<Vec<crate::transition::FileTransition>> {
        Ok(Vec::new())
    }

    fn transitions_by_source(
        &self,
        _source: crate::transition::TransitionSource,
    ) -> Result<Vec<crate::transition::FileTransition>> {
        Ok(Vec::new())
    }
}
```

- [ ] **Step 7: Update `StorageTrait` if it bundles sub-traits**

Check `StorageTrait` definition in `storage.rs`. If it includes `FileHistoryStorage`, replace with `FileTransitionStorage`. Also remove `delete_file` references if `StorageTrait` has them.

- [ ] **Step 8: Fix all compilation errors**

Run `cargo build 2>&1 | head -80` and fix:
- Any code calling `delete_file()` — replace with `mark_missing()`
- Any code referencing `FileHistoryStorage` — replace with `FileTransitionStorage`
- Any code referencing `FileHistoryEntry` or `StoredHistoryRow` — remove

Key files to check:
- `plugins/sqlite-store/src/store/file_history_storage.rs` — delete this file entirely
- `plugins/sqlite-store/src/store/mod.rs:4` — remove `mod file_history_storage;`
- `plugins/sqlite-store/src/lib.rs` — update trait impls
- `crates/voom-cli/src/commands/` — any `delete_file` calls
- `plugins/sqlite-store/src/store/maintenance_storage.rs` — `prune_missing_files_under` now becomes `mark_missing`-based

- [ ] **Step 9: Run tests**

Run: `cargo test`
Expected: All tests pass (some tests that tested `delete_file` or `file_history` may need updating).

- [ ] **Step 10: Commit**

```bash
git add -u
git add crates/voom-domain/src/storage.rs crates/voom-domain/src/lib.rs crates/voom-domain/src/test_support.rs
git commit -m "feat: replace FileHistoryStorage with FileTransitionStorage, update FileStorage trait"
```

---

### Task 3: Update SQLite schema and storage implementation

**Files:**
- Modify: `plugins/sqlite-store/src/schema.rs:77-88` (replace `file_history` with `file_transitions`)
- Modify: `plugins/sqlite-store/src/schema.rs:4-192` (add columns to `files`)
- Delete: `plugins/sqlite-store/src/store/file_history_storage.rs`
- Create: `plugins/sqlite-store/src/store/file_transition_storage.rs`
- Modify: `plugins/sqlite-store/src/store/mod.rs`
- Modify: `plugins/sqlite-store/src/store/row_mappers.rs`
- Modify: `plugins/sqlite-store/src/store/file_storage.rs`
- Modify: `plugins/sqlite-store/src/store/maintenance_storage.rs`

- [ ] **Step 1: Update schema SQL**

In `plugins/sqlite-store/src/schema.rs`, replace the `file_history` table and index (lines 77-88) with:

```sql
CREATE TABLE IF NOT EXISTS file_transitions (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    path TEXT NOT NULL,
    from_hash TEXT,
    to_hash TEXT NOT NULL,
    from_size INTEGER,
    to_size INTEGER NOT NULL,
    source TEXT NOT NULL,
    source_detail TEXT,
    plan_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transitions_file ON file_transitions(file_id);
CREATE INDEX IF NOT EXISTS idx_transitions_source ON file_transitions(source);
```

In the `files` table definition (lines 5-19), change `path TEXT NOT NULL UNIQUE` to `path TEXT UNIQUE` and add after `content_hash`:

```sql
    expected_hash TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    missing_since TEXT,
```

- [ ] **Step 2: Update migration function**

In `plugins/sqlite-store/src/schema.rs` `migrate()`, add migrations for existing databases:
- Add `expected_hash`, `status`, `missing_since` columns to `files` if missing.
- Create `file_transitions` table if missing.
- Drop `file_history` table if it exists.

```rust
    // Migrate file_history → file_transitions
    let has_file_history: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='file_history'",
        [],
        |row| row.get(0),
    )?;
    if has_file_history {
        conn.execute_batch("DROP TABLE IF EXISTS file_history")?;
    }

    let has_file_transitions: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='file_transitions'",
        [],
        |row| row.get(0),
    )?;
    if !has_file_transitions {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_transitions (
                id TEXT PRIMARY KEY,
                file_id TEXT NOT NULL,
                path TEXT NOT NULL,
                from_hash TEXT,
                to_hash TEXT NOT NULL,
                from_size INTEGER,
                to_size INTEGER NOT NULL,
                source TEXT NOT NULL,
                source_detail TEXT,
                plan_id TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_transitions_file ON file_transitions(file_id);
            CREATE INDEX IF NOT EXISTS idx_transitions_source ON file_transitions(source);",
        )?;
    }

    if !has_column("files", "expected_hash")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN expected_hash TEXT")?;
    }
    if !has_column("files", "status")? {
        conn.execute_batch(
            "ALTER TABLE files ADD COLUMN status TEXT NOT NULL DEFAULT 'active'"
        )?;
    }
    if !has_column("files", "missing_since")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN missing_since TEXT")?;
    }
```

- [ ] **Step 3: Update `row_mappers.rs` — add `expected_hash`, `status` to `FileRow`**

In `plugins/sqlite-store/src/store/row_mappers.rs`, add fields to `FileRow`:

```rust
    pub expected_hash: Option<String>,
    pub status: String,
```

Update `row_to_file` to read the new columns from the SELECT. Update `FileRow::to_media_file()` to populate `expected_hash` and `status` on the returned `MediaFile`.

Note: Some queries (like the SELECT in `file_storage.rs`) will need the new columns added to their column list.

- [ ] **Step 4: Update `file_storage.rs` — new columns in queries, replace `delete_file` with `mark_missing`**

In `plugins/sqlite-store/src/store/file_storage.rs`:

Update `upsert_file` to include `expected_hash`, `status`, `missing_since` in the INSERT and ON CONFLICT UPDATE. Remove the `file_history` archive INSERT (lines 48-58) — transitions are recorded by callers now, not by upsert.

Replace `delete_file` with:

```rust
    fn mark_missing(&self, id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        conn.execute(
            "UPDATE files SET status = 'missing', missing_since = ?1 WHERE id = ?2 AND status = 'active'",
            params![now, id.to_string()],
        )
        .map_err(storage_err("failed to mark file missing"))?;
        Ok(())
    }
```

Add `reactivate_file`:

```rust
    fn reactivate_file(&self, id: &Uuid, new_path: &Path) -> Result<()> {
        let conn = self.conn()?;
        let now = format_datetime(&Utc::now());
        let path_str = new_path.to_string_lossy().to_string();
        let filename = new_path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        conn.execute(
            "UPDATE files SET status = 'active', missing_since = NULL, path = ?1, filename = ?2, updated_at = ?3 WHERE id = ?4",
            params![path_str, filename, now, id.to_string()],
        )
        .map_err(storage_err("failed to reactivate file"))?;
        Ok(())
    }
```

Add `purge_missing`:

```rust
    fn purge_missing(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let conn = self.conn()?;
        let cutoff = format_datetime(&older_than);
        // Delete transitions for files about to be purged
        conn.execute(
            "DELETE FROM file_transitions WHERE file_id IN (SELECT id FROM files WHERE status = 'missing' AND missing_since < ?1)",
            params![cutoff],
        )
        .map_err(storage_err("failed to purge transitions"))?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM files WHERE status = 'missing' AND missing_since < ?1",
                params![cutoff],
            )
            .map_err(storage_err("failed to purge missing files"))?;
        Ok(deleted as u64)
    }
```

Add a stub `reconcile_discovered_files` — the real implementation comes in Task 5:

```rust
    fn reconcile_discovered_files(
        &self,
        _discovered: &[voom_domain::transition::DiscoveredFile],
        _scanned_dirs: &[std::path::PathBuf],
    ) -> Result<voom_domain::transition::ReconcileResult> {
        Ok(voom_domain::transition::ReconcileResult::default())
    }
```

- [ ] **Step 5: Update `list_files` and `count_files` to filter by status**

In `file_storage.rs`, add a status filter to `list_files` and `count_files`. When `filters.include_missing` is false (default), add `AND {col_prefix}status = 'active'` to the WHERE clause.

- [ ] **Step 6: Delete `file_history_storage.rs`, create `file_transition_storage.rs`**

Delete `plugins/sqlite-store/src/store/file_history_storage.rs`.

Create `plugins/sqlite-store/src/store/file_transition_storage.rs`:

```rust
use rusqlite::params;
use uuid::Uuid;

use voom_domain::errors::Result;
use voom_domain::storage::FileTransitionStorage;
use voom_domain::transition::{FileTransition, TransitionSource};

use super::{format_datetime, parse_required_datetime, row_uuid, storage_err, SqliteStore};

impl FileTransitionStorage for SqliteStore {
    fn record_transition(&self, t: &FileTransition) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO file_transitions (id, file_id, path, from_hash, to_hash, from_size, to_size, source, source_detail, plan_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                t.id.to_string(),
                t.file_id.to_string(),
                t.path.to_string_lossy().to_string(),
                t.from_hash.as_deref().unwrap_or(""),
                t.to_hash,
                t.from_size.map(|v| v as i64),
                t.to_size as i64,
                t.source.as_str(),
                t.source_detail.as_deref().unwrap_or(""),
                t.plan_id.map(|id| id.to_string()).unwrap_or_default(),
                format_datetime(&t.created_at),
            ],
        )
        .map_err(storage_err("failed to record transition"))?;
        Ok(())
    }

    fn transitions_for_file(&self, file_id: &Uuid) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_hash, to_hash, from_size, to_size, source, source_detail, plan_id, created_at
                 FROM file_transitions WHERE file_id = ?1 ORDER BY created_at",
            )
            .map_err(storage_err("failed to prepare transitions query"))?;

        let rows = stmt
            .query_map(params![file_id.to_string()], row_to_transition)
            .map_err(storage_err("failed to query transitions"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect transitions"))?;

        Ok(rows)
    }

    fn transitions_by_source(&self, source: TransitionSource) -> Result<Vec<FileTransition>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_id, path, from_hash, to_hash, from_size, to_size, source, source_detail, plan_id, created_at
                 FROM file_transitions WHERE source = ?1 ORDER BY created_at",
            )
            .map_err(storage_err("failed to prepare transitions-by-source query"))?;

        let rows = stmt
            .query_map(params![source.as_str()], row_to_transition)
            .map_err(storage_err("failed to query transitions by source"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect transitions"))?;

        Ok(rows)
    }
}

fn row_to_transition(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTransition> {
    let id_str: String = row.get("id")?;
    let file_id_str: String = row.get("file_id")?;
    let source_str: String = row.get("source")?;
    let plan_id_str: String = row.get("plan_id")?;
    let created_str: String = row.get("created_at")?;
    let from_hash: String = row.get("from_hash")?;
    let source_detail: String = row.get("source_detail")?;

    Ok(FileTransition {
        id: row_uuid(&id_str, "file_transitions")?,
        file_id: row_uuid(&file_id_str, "file_transitions")?,
        path: std::path::PathBuf::from(row.get::<_, String>("path")?),
        from_hash: if from_hash.is_empty() {
            None
        } else {
            Some(from_hash)
        },
        to_hash: row.get("to_hash")?,
        from_size: {
            let v: Option<i64> = row.get("from_size")?;
            v.map(|n| n as u64)
        },
        to_size: row.get::<_, i64>("to_size")? as u64,
        source: TransitionSource::from_str_lossy(&source_str),
        source_detail: if source_detail.is_empty() {
            None
        } else {
            Some(source_detail)
        },
        plan_id: if plan_id_str.is_empty() {
            None
        } else {
            Some(row_uuid(&plan_id_str, "file_transitions")?)
        },
        created_at: parse_required_datetime(&created_str, "file_transitions.created_at")?,
    })
}
```

- [ ] **Step 7: Update `mod.rs` — swap module declarations**

In `plugins/sqlite-store/src/store/mod.rs`, replace `mod file_history_storage;` (line 4) with `mod file_transition_storage;`.

- [ ] **Step 8: Update `maintenance_storage.rs` — use `mark_missing` instead of DELETE**

Replace `prune_missing_files_under` to mark files missing instead of deleting them. The method should:
1. Query active files under root.
2. Check filesystem existence.
3. For each missing file, call `mark_missing` (or do a batch UPDATE).
4. Return the count of newly-marked files.

Remove the `chunked_delete` calls for Plans/ProcessingStats/Files — those happen during `purge_missing`.

Also update `table_row_counts` (line 89) to reference `file_transitions` instead of `file_history`.

- [ ] **Step 9: Update `sqlite-store/src/lib.rs` trait bounds**

Ensure the `Plugin` impl for `SqliteStore` and any event handlers reference the new traits. The `FileDiscovered` event handler may need updating — it currently upserts to `discovered_files` staging table, which stays.

- [ ] **Step 10: Run tests, fix breakage**

Run: `cargo test`
Fix any remaining compilation errors or test failures. Schema tests in `schema.rs` will need updating to check for `file_transitions` instead of `file_history`.

- [ ] **Step 11: Commit**

```bash
git add -u
git add plugins/sqlite-store/src/store/file_transition_storage.rs
git commit -m "feat: implement file_transitions schema and storage, replace file_history"
```

---

### Task 4: Add config sections for recovery and pruning

**Files:**
- Modify: `crates/voom-cli/src/config.rs:12-32`

- [ ] **Step 1: Add config types**

In `crates/voom-cli/src/config.rs`, add after the imports:

```rust
/// Crash recovery configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryConfig {
    /// How to handle orphaned backups from crashed executions.
    /// Values: "always_recover", "always_discard", "prompt"
    #[serde(default = "default_recovery_mode")]
    pub mode: String,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            mode: default_recovery_mode(),
        }
    }
}

fn default_recovery_mode() -> String {
    "prompt".into()
}

/// Missing file pruning configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PruningConfig {
    /// Days to keep missing file records before purging.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
        }
    }
}

fn default_retention_days() -> u32 {
    30
}
```

Add to `AppConfig` struct:

```rust
    #[serde(default)]
    pub recovery: RecoveryConfig,
    #[serde(default)]
    pub pruning: PruningConfig,
```

- [ ] **Step 2: Update default config template**

In `default_config_contents()`, add sections for the new config:

```toml
# Crash recovery: what to do with orphaned backups from interrupted executions.
# mode = "always_recover" | "always_discard" | "prompt"
[recovery]
mode = "prompt"

# Missing file pruning: how long to keep records for files no longer on disk.
[pruning]
retention_days = 30
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p voom-cli`
Expected: Config loading tests pass with new defaults.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/src/config.rs
git commit -m "feat: add recovery and pruning config sections"
```

---

### Task 5: Implement batch reconciliation

**Files:**
- Modify: `plugins/sqlite-store/src/store/file_storage.rs`
- Test: `plugins/sqlite-store/src/store/mod.rs` (test module)

This is the core identity resolution logic. It replaces the stub from Task 3.

- [ ] **Step 1: Write reconciliation tests**

Add tests to the test module in `plugins/sqlite-store/src/store/mod.rs`:

```rust
#[test]
fn reconcile_new_file() {
    // Setup: empty DB
    // Discover one file
    // Assert: ReconcileResult { new_files: 1, ..default }
    // Assert: file exists in DB with status=active, expected_hash set
    // Assert: one transition with source=discovery
}

#[test]
fn reconcile_unchanged_file() {
    // Setup: file in DB with expected_hash = "abc"
    // Discover same file with hash "abc"
    // Assert: ReconcileResult { unchanged: 1, ..default }
    // Assert: no new transitions recorded
}

#[test]
fn reconcile_external_modification() {
    // Setup: file in DB at /test.mkv with expected_hash = "abc"
    // Discover /test.mkv with hash "xyz"
    // Assert: ReconcileResult { external_changes: 1, ..default }
    // Assert: old file marked missing, path NULLed
    // Assert: new file at /test.mkv with different UUID
    // Assert: external transition on old file_id
    // Assert: discovery transition on new file_id
}

#[test]
fn reconcile_missing_file() {
    // Setup: file in DB at /test.mkv
    // Discover nothing under /
    // Assert: ReconcileResult { missing: 1, ..default }
    // Assert: file status=missing, missing_since set
}

#[test]
fn reconcile_move_detected() {
    // Setup: file in DB at /old.mkv with expected_hash = "abc", mark it missing
    // Discover /new.mkv with hash "abc"
    // Assert: ReconcileResult { moved: 1, ..default }
    // Assert: file reactivated at /new.mkv with original UUID
    // Assert: discovery transition with source_detail="detected_move"
}

#[test]
fn reconcile_scoped_to_scanned_dirs() {
    // Setup: file in DB at /movies/a.mkv, file at /tv/b.mkv
    // Discover nothing, but scanned_dirs = [/movies/]
    // Assert: /movies/a.mkv marked missing
    // Assert: /tv/b.mkv still active (not in scanned dirs)
}

#[test]
fn reconcile_reappeared_file_with_matching_hash() {
    // Setup: file in DB at /test.mkv, status=missing, expected_hash="abc"
    // Discover /test.mkv with hash "abc"
    // Assert: file reactivated (status=active, missing_since=NULL)
    // Assert: ReconcileResult { unchanged: 1 } (or a dedicated counter)
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p sqlite-store -- reconcile`
Expected: All fail (stub returns default).

- [ ] **Step 3: Implement `reconcile_discovered_files`**

Replace the stub in `file_storage.rs` with the full two-pass implementation:

```rust
fn reconcile_discovered_files(
    &self,
    discovered: &[voom_domain::transition::DiscoveredFile],
    scanned_dirs: &[std::path::PathBuf],
) -> Result<voom_domain::transition::ReconcileResult> {
    use voom_domain::transition::{
        FileTransition, ReconcileResult, TransitionSource,
    };

    let mut conn = self.conn()?;
    let now = format_datetime(&Utc::now());
    let tx = conn
        .transaction()
        .map_err(storage_err("failed to begin reconcile transaction"))?;

    let mut result = ReconcileResult::default();

    // Build a set of discovered paths for fast lookup
    let discovered_paths: std::collections::HashSet<String> = discovered
        .iter()
        .map(|d| d.path.to_string_lossy().to_string())
        .collect();

    // Pass 1: Mark missing — only files under scanned directories
    {
        let mut stmt = tx
            .prepare("SELECT id, path FROM files WHERE status = 'active'")
            .map_err(storage_err("failed to prepare missing check"))?;
        let active_files: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage_err("failed to query active files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect active files"))?;

        for (id, path) in &active_files {
            let under_scanned = scanned_dirs
                .iter()
                .any(|dir| path.starts_with(&dir.to_string_lossy().as_ref()));
            if under_scanned && !discovered_paths.contains(path.as_str()) {
                tx.execute(
                    "UPDATE files SET status = 'missing', missing_since = ?1 WHERE id = ?2",
                    params![&now, id],
                )
                .map_err(storage_err("failed to mark missing"))?;
                result.missing += 1;
            }
        }
    }

    // Pass 2: Match discovered files
    for d in discovered {
        let path_str = d.path.to_string_lossy().to_string();

        // Check if path exists in DB (active or missing)
        let existing: Option<(String, Option<String>, String, Option<String>)> = tx
            .query_row(
                "SELECT id, expected_hash, status, content_hash FROM files WHERE path = ?1",
                params![&path_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(storage_err("failed to check existing file"))?;

        if let Some((existing_id, expected_hash, status, _content_hash)) = existing {
            let hash_matches = expected_hash
                .as_deref()
                .map_or(true, |eh| eh == d.content_hash);

            if hash_matches {
                // Unchanged (or reappeared with matching hash)
                if status == "missing" {
                    // Reactivate
                    tx.execute(
                        "UPDATE files SET status = 'active', missing_since = NULL, updated_at = ?1 WHERE id = ?2",
                        params![&now, &existing_id],
                    )
                    .map_err(storage_err("failed to reactivate file"))?;
                }
                result.unchanged += 1;
            } else {
                // External modification — different hash at same path
                // Write external transition on old file
                let old_file_id = parse_uuid(&existing_id)?;
                let ext_transition = FileTransition::new(
                    old_file_id,
                    d.path.clone(),
                    expected_hash,
                    d.content_hash.clone(),
                    None, // from_size unknown without querying
                    d.size,
                    TransitionSource::External,
                );
                insert_transition(&tx, &ext_transition, &now)?;

                // NULL out path on old record, mark missing
                tx.execute(
                    "UPDATE files SET path = NULL, status = 'missing', missing_since = ?1 WHERE id = ?2",
                    params![&now, &existing_id],
                )
                .map_err(storage_err("failed to mark externally replaced file"))?;

                // Create new file record
                let new_id = Uuid::new_v4().to_string();
                let filename = d.path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                tx.execute(
                    "INSERT INTO files (id, path, filename, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', 'other', 0.0, NULL, '{}', '{}', ?7, ?7, ?7)",
                    params![&new_id, &path_str, filename, d.size as i64, &d.content_hash, &d.content_hash, &now],
                )
                .map_err(storage_err("failed to create new file for external change"))?;

                // Discovery transition on new file
                let new_uuid = parse_uuid(&new_id)?;
                let disc_transition = FileTransition::new(
                    new_uuid,
                    d.path.clone(),
                    None,
                    d.content_hash.clone(),
                    None,
                    d.size,
                    TransitionSource::Discovery,
                );
                insert_transition(&tx, &disc_transition, &now)?;

                result.external_changes += 1;
            }
        } else {
            // No record at this path — check for move (missing file with matching hash)
            let moved_from: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT id, path FROM files WHERE status = 'missing' AND expected_hash = ?1 LIMIT 1",
                    params![&d.content_hash],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(storage_err("failed to check for moved file"))?;

            if let Some((moved_id, old_path)) = moved_from {
                // Move detected — reactivate at new path
                let filename = d.path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                tx.execute(
                    "UPDATE files SET path = ?1, filename = ?2, status = 'active', missing_since = NULL, size = ?3, content_hash = ?4, updated_at = ?5 WHERE id = ?6",
                    params![&path_str, filename, d.size as i64, &d.content_hash, &now, &moved_id],
                )
                .map_err(storage_err("failed to reactivate moved file"))?;

                let moved_uuid = parse_uuid(&moved_id)?;
                let move_transition = FileTransition::new(
                    moved_uuid,
                    d.path.clone(),
                    old_path.map(|p| format!("moved_from:{p}")),
                    d.content_hash.clone(),
                    None,
                    d.size,
                    TransitionSource::Discovery,
                )
                .with_detail("detected_move");
                insert_transition(&tx, &move_transition, &now)?;

                result.moved += 1;
            } else {
                // Genuinely new file
                let new_id = Uuid::new_v4().to_string();
                let filename = d.path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                tx.execute(
                    "INSERT INTO files (id, path, filename, size, content_hash, expected_hash, status, container, duration, bitrate, tags, plugin_metadata, introspected_at, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', 'other', 0.0, NULL, '{}', '{}', ?7, ?7, ?7)",
                    params![&new_id, &path_str, filename, d.size as i64, &d.content_hash, &d.content_hash, &now],
                )
                .map_err(storage_err("failed to insert new file"))?;

                let new_uuid = parse_uuid(&new_id)?;
                let disc_transition = FileTransition::new(
                    new_uuid,
                    d.path.clone(),
                    None,
                    d.content_hash.clone(),
                    None,
                    d.size,
                    TransitionSource::Discovery,
                );
                insert_transition(&tx, &disc_transition, &now)?;

                result.new_files += 1;
            }
        }
    }

    tx.commit()
        .map_err(storage_err("failed to commit reconciliation"))?;
    Ok(result)
}
```

Add the helper function in the same file:

```rust
fn insert_transition(
    tx: &rusqlite::Transaction<'_>,
    t: &voom_domain::transition::FileTransition,
    now: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO file_transitions (id, file_id, path, from_hash, to_hash, from_size, to_size, source, source_detail, plan_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            t.id.to_string(),
            t.file_id.to_string(),
            t.path.to_string_lossy().to_string(),
            t.from_hash.as_deref().unwrap_or(""),
            t.to_hash,
            t.from_size.map(|v| v as i64),
            t.to_size as i64,
            t.source.as_str(),
            t.source_detail.as_deref().unwrap_or(""),
            t.plan_id.map(|id| id.to_string()).unwrap_or_default(),
            now,
        ],
    )
    .map_err(storage_err("failed to insert transition"))?;
    Ok(())
}
```

- [ ] **Step 4: Run reconciliation tests**

Run: `cargo test -p sqlite-store -- reconcile`
Expected: All 7 tests pass.

- [ ] **Step 5: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat: implement batch reconciliation with move detection and external change tracking"
```

---

### Task 6: Update scan command to use reconciliation

**Files:**
- Modify: `crates/voom-cli/src/commands/scan.rs`

- [ ] **Step 1: Replace auto-prune + per-file dispatch with reconciliation call**

In `scan.rs`, replace the auto-prune block (lines 30-39) and the `FileDiscovered` dispatch loop (lines 209-211) with a single reconciliation call.

After `all_events` is deduplicated (line 131) and before introspection begins:

```rust
    // Convert events to DiscoveredFile for reconciliation
    let discovered: Vec<voom_domain::DiscoveredFile> = all_events
        .iter()
        .map(|e| voom_domain::DiscoveredFile {
            path: e.path.clone(),
            size: e.size,
            content_hash: e.content_hash.clone().unwrap_or_default(),
        })
        .collect();

    // Batch reconciliation: mark missing, detect moves, identify external changes
    let reconcile_result = store.reconcile_discovered_files(&discovered, &paths)?;

    if !quiet {
        if reconcile_result.missing > 0 {
            eprintln!(
                "  {} {} files no longer on disk",
                style("Missing").dim(),
                reconcile_result.missing,
            );
        }
        if reconcile_result.moved > 0 {
            eprintln!(
                "  {} {} files moved/renamed",
                style("Moved").dim(),
                reconcile_result.moved,
            );
        }
        if reconcile_result.external_changes > 0 {
            eprintln!(
                "  {} {} files changed externally",
                style("Changed").dim(),
                reconcile_result.external_changes,
            );
        }
    }
```

Remove the old auto-prune block (lines 30-39).

Keep the `FileDiscovered` event dispatch (lines 209-211) for subscribers that need it (discovered_files staging, job enqueue), but introspection should only run for files that need it (new + externally changed + moved):

```rust
    // Only introspect files that are new, externally changed, or moved
    // For unchanged files, skip introspection
    let needs_introspection: Vec<_> = all_events
        .iter()
        .filter(|e| {
            // If file existed with matching hash, it was counted as unchanged
            // and doesn't need re-introspection. We check by querying the store.
            store
                .file_by_path(&e.path)
                .ok()
                .flatten()
                .map_or(true, |f| {
                    f.expected_hash.as_deref() != e.content_hash.as_deref()
                })
        })
        .collect();
```

Use `needs_introspection` instead of `all_events` for the introspection loop.

- [ ] **Step 2: Add pruning at end of scan**

After introspection completes, run time-based pruning:

```rust
    // Prune file records that have been missing longer than retention window
    let retention_days = config.pruning.retention_days;
    if retention_days > 0 {
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(retention_days));
        match store.purge_missing(cutoff) {
            Ok(n) if n > 0 && !quiet => {
                eprintln!(
                    "  {} {} stale records (missing >{} days)",
                    style("Purged").dim(),
                    n,
                    retention_days,
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "purge failed"),
        }
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p voom-cli`
Expected: Scan-related tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/src/commands/scan.rs
git commit -m "feat: scan uses batch reconciliation with move detection and pruning"
```

---

### Task 7: Record transitions after plan execution

**Files:**
- Modify: `crates/voom-cli/src/commands/process.rs:707-905`

- [ ] **Step 1: Record transition after each successful phase**

In `process_single_file_execute`, after `reintrospect_file` returns successfully (around line 886), record a transition and update `expected_hash`:

```rust
                // Re-introspect so the next phase sees updated file state
                let new_file = reintrospect_file(&current_file, &[plan], ctx).await;

                // Record voom transition
                let transition = voom_domain::FileTransition::new(
                    current_file.id,
                    new_file.path.clone(),
                    current_file.content_hash.clone(),
                    new_file.content_hash.clone().unwrap_or_default(),
                    Some(current_file.size),
                    new_file.size,
                    voom_domain::TransitionSource::Voom,
                )
                .with_detail(format!(
                    "{}:{}",
                    plan.executor_plugin.as_deref().unwrap_or("unknown"),
                    plan.phase_name
                ))
                .with_plan_id(plan.id);

                if let Some(ref store) = ctx.store {
                    if let Err(e) = store.record_transition(&transition) {
                        tracing::warn!(error = %e, "failed to record transition");
                    }
                    // Update expected_hash so next scan recognizes this as a voom change
                    if let Some(ref hash) = new_file.content_hash {
                        if let Err(e) = store.update_expected_hash(&current_file.id, hash) {
                            tracing::warn!(error = %e, "failed to update expected_hash");
                        }
                    }
                }

                current_file = new_file;
```

Note: This requires:
1. The `ProcessContext` to have access to the store (as a `&dyn FileTransitionStorage` or the concrete store type). Check how `ctx` is structured and add the store reference if needed.
2. An `update_expected_hash` method on `FileStorage` or a direct SQL call. Add to the trait:

```rust
fn update_expected_hash(&self, id: &Uuid, hash: &str) -> Result<()>;
```

With implementation:

```rust
fn update_expected_hash(&self, id: &Uuid, hash: &str) -> Result<()> {
    let conn = self.conn()?;
    conn.execute(
        "UPDATE files SET expected_hash = ?1 WHERE id = ?2",
        params![hash, id.to_string()],
    )
    .map_err(storage_err("failed to update expected_hash"))?;
    Ok(())
}
```

- [ ] **Step 2: Check how `ProcessContext` accesses storage**

The `ProcessContext` struct likely has a `kernel` but may not directly hold a store reference. Check the struct definition and either:
- Add `store: Arc<dyn StorageTrait>` to `ProcessContext`
- Or access storage through the kernel's plugin system

Adapt the transition recording code to use whatever access pattern exists.

- [ ] **Step 3: Check `Plan` struct for `executor_plugin` field**

The `source_detail` format requires knowing which executor ran the plan. Check if `Plan` has an `executor_plugin` field. If not, the executor info may need to come from the `EventResult` of `execute_single_plan`, or be inferred from the plan's capability requirements. Adapt the `source_detail` construction accordingly.

- [ ] **Step 4: Run tests**

Run: `cargo test -p voom-cli`
Expected: Process-related tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/voom-cli/src/commands/process.rs
git add -u  # trait changes
git commit -m "feat: record file transitions after plan execution"
```

---

### Task 8: Implement crash recovery

**Files:**
- Create: `crates/voom-cli/src/recovery.rs`
- Modify: `crates/voom-cli/src/commands/process.rs` (call recovery on startup)
- Modify: `crates/voom-cli/src/lib.rs` or `mod.rs` (add module)

- [ ] **Step 1: Write the recovery module**

Create `crates/voom-cli/src/recovery.rs`:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::RecoveryConfig;

/// An orphaned backup discovered during recovery check.
#[derive(Debug)]
pub struct OrphanedBackup {
    pub original_path: PathBuf,
    pub backup_path: PathBuf,
    pub size: u64,
}

/// Check for orphaned .vbak files and resolve them per config.
///
/// Returns the number of files recovered/discarded.
pub fn check_and_recover(
    config: &RecoveryConfig,
    backup_dirs: &[PathBuf],
    store: &dyn voom_domain::storage::FileTransitionStorage,
) -> Result<u64> {
    let orphans = find_orphaned_backups(backup_dirs)?;
    if orphans.is_empty() {
        return Ok(0);
    }

    let mut resolved = 0u64;
    for orphan in &orphans {
        match config.mode.as_str() {
            "always_recover" => {
                recover_file(orphan, store)?;
                resolved += 1;
            }
            "always_discard" => {
                discard_backup(orphan, store)?;
                resolved += 1;
            }
            "prompt" => {
                // In non-interactive mode, log and skip
                tracing::warn!(
                    path = %orphan.original_path.display(),
                    backup = %orphan.backup_path.display(),
                    "orphaned backup found — run interactively or set recovery.mode in config"
                );
            }
            other => {
                tracing::warn!(mode = other, "unknown recovery mode, skipping");
            }
        }
    }

    Ok(resolved)
}

fn find_orphaned_backups(backup_dirs: &[PathBuf]) -> Result<Vec<OrphanedBackup>> {
    let mut orphans = Vec::new();
    for dir in backup_dirs {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("reading backup dir {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "vbak") {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                // Infer original path from backup filename
                let original = infer_original_path(&path);
                orphans.push(OrphanedBackup {
                    original_path: original,
                    backup_path: path,
                    size,
                });
            }
        }
    }
    Ok(orphans)
}

fn infer_original_path(backup_path: &Path) -> PathBuf {
    // .vbak files are named like: <original_name>.<uuid>.vbak
    // or in global mode: <hash>_<original_name>
    // Strip the .vbak and UUID suffix to recover the original name
    let stem = backup_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // Remove trailing .<uuid> if present
    if let Some(dot_pos) = stem.rfind('.') {
        let potential_uuid = &stem[dot_pos + 1..];
        if potential_uuid.len() == 36 && potential_uuid.contains('-') {
            let original_name = &stem[..dot_pos];
            return backup_path
                .parent()
                .unwrap_or(Path::new("/"))
                .parent()
                .unwrap_or(Path::new("/"))
                .join(original_name);
        }
    }
    backup_path.with_extension("")
}

fn recover_file(
    orphan: &OrphanedBackup,
    store: &dyn voom_domain::storage::FileTransitionStorage,
) -> Result<()> {
    std::fs::copy(&orphan.backup_path, &orphan.original_path)
        .with_context(|| {
            format!(
                "restoring {} from {}",
                orphan.original_path.display(),
                orphan.backup_path.display()
            )
        })?;
    std::fs::remove_file(&orphan.backup_path)?;

    // Record unknown transition
    let hash = voom_discovery::hash_file(&orphan.original_path)
        .unwrap_or_else(|_| String::new());
    let size = std::fs::metadata(&orphan.original_path)
        .map(|m| m.len())
        .unwrap_or(0);

    // We need the file_id — look it up by path
    // This may not exist if the file was never fully recorded
    // In that case, skip the transition recording
    let transition = voom_domain::FileTransition::new(
        uuid::Uuid::nil(), // placeholder — caller should resolve file_id
        orphan.original_path.clone(),
        None,
        hash,
        None,
        size,
        voom_domain::TransitionSource::Unknown,
    )
    .with_detail("crash_recovery:restored");

    // Best-effort transition recording
    let _ = store.record_transition(&transition);

    tracing::info!(
        path = %orphan.original_path.display(),
        "recovered from backup"
    );
    Ok(())
}

fn discard_backup(
    orphan: &OrphanedBackup,
    store: &dyn voom_domain::storage::FileTransitionStorage,
) -> Result<()> {
    std::fs::remove_file(&orphan.backup_path)
        .with_context(|| format!("removing backup {}", orphan.backup_path.display()))?;

    let hash = if orphan.original_path.exists() {
        voom_discovery::hash_file(&orphan.original_path).unwrap_or_default()
    } else {
        String::new()
    };
    let size = std::fs::metadata(&orphan.original_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let transition = voom_domain::FileTransition::new(
        uuid::Uuid::nil(),
        orphan.original_path.clone(),
        None,
        hash,
        None,
        size,
        voom_domain::TransitionSource::Unknown,
    )
    .with_detail("crash_recovery:discarded");

    let _ = store.record_transition(&transition);

    tracing::info!(
        path = %orphan.original_path.display(),
        "discarded orphaned backup"
    );
    Ok(())
}
```

- [ ] **Step 2: Wire recovery into process command**

In `process.rs`, before plan evaluation begins, call:

```rust
    // Check for orphaned backups from crashed executions
    let backup_dirs = collect_backup_dirs(&kernel);
    let recovered = crate::recovery::check_and_recover(
        &config.recovery,
        &backup_dirs,
        store.as_ref(),
    )?;
    if recovered > 0 && !quiet {
        eprintln!(
            "{} {} files from crashed execution",
            style("Recovered").bold().green(),
            recovered,
        );
    }
```

Add the `recovery` module to `crates/voom-cli/src/lib.rs` or the appropriate mod file.

- [ ] **Step 3: Run tests**

Run: `cargo test -p voom-cli`
Expected: Tests pass. Recovery module compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/src/recovery.rs
git add -u
git commit -m "feat: add crash recovery for orphaned backups"
```

---

### Task 9: Update maintenance command and final cleanup

**Files:**
- Modify: `plugins/sqlite-store/src/store/maintenance_storage.rs`
- Modify: `crates/voom-cli/src/commands/` (db maintenance command)

- [ ] **Step 1: Update `prune_missing_files_under` to use soft-delete**

In `maintenance_storage.rs`, the method should now mark files as missing (not delete them). The actual deletion happens through `purge_missing`. Update:

```rust
fn prune_missing_files_under(&self, root: &Path) -> Result<u64> {
    let root_str = escape_like(&root.to_string_lossy());

    // Prune bad_files whose paths no longer exist under root (still hard-delete)
    {
        let bad_files: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT id, path FROM bad_files WHERE path LIKE ?1 || '%' ESCAPE '\\'")
                .map_err(storage_err("failed to prepare bad_files prune"))?;
            stmt.query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(storage_err("failed to query bad_files"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err("failed to collect bad_files"))?
        };
        let missing_bad_ids: Vec<&str> = bad_files
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .map(|(id, _)| id.as_str())
            .collect();
        self.chunked_delete(PruneTarget::BadFiles, &missing_bad_ids)?;
    }

    // Mark missing files as soft-deleted instead of hard-deleting
    let conn = self.conn()?;
    let now = super::format_datetime(&chrono::Utc::now());
    let marked: usize = conn
        .execute(
            "UPDATE files SET status = 'missing', missing_since = ?1
             WHERE status = 'active'
               AND path LIKE ?2 || '%' ESCAPE '\\'
               AND path NOT IN (SELECT path FROM files WHERE path IS NOT NULL AND status = 'active')",
            params![&now, &root_str],
        )
        .map_err(storage_err("failed to mark missing files"))?;

    Ok(marked as u64)
}
```

Actually, `prune_missing_files_under` checks filesystem existence. Keep that logic but use UPDATE instead of DELETE:

```rust
fn prune_missing_files_under(&self, root: &Path) -> Result<u64> {
    let root_str = escape_like(&root.to_string_lossy());

    // Bad files: still hard-delete
    {
        let bad_files: Vec<(String, String)> = {
            let conn = self.conn()?;
            let mut stmt = conn
                .prepare("SELECT id, path FROM bad_files WHERE path LIKE ?1 || '%' ESCAPE '\\'")
                .map_err(storage_err("failed to prepare bad_files prune"))?;
            stmt.query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(storage_err("failed to query bad_files"))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage_err("failed to collect bad_files"))?
        };
        let missing_bad_ids: Vec<&str> = bad_files
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .map(|(id, _)| id.as_str())
            .collect();
        self.chunked_delete(PruneTarget::BadFiles, &missing_bad_ids)?;
    }

    // Files: soft-delete (mark missing)
    let files: Vec<(String, String)> = {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT id, path FROM files WHERE status = 'active' AND path LIKE ?1 || '%' ESCAPE '\\'")
            .map_err(storage_err("failed to prepare prune query"))?;
        stmt.query_map(params![root_str], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(storage_err("failed to query files"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect files"))?
    };

    let missing_ids: Vec<&str> = files
        .iter()
        .filter(|(_, path)| !Path::new(path).exists())
        .map(|(id, _)| id.as_str())
        .collect();

    if missing_ids.is_empty() {
        return Ok(0);
    }

    let conn = self.conn()?;
    let now = super::format_datetime(&chrono::Utc::now());
    let mut marked = 0u64;
    for chunk in missing_ids.chunks(500) {
        let placeholders: Vec<String> = chunk.iter().enumerate().map(|(i, _)| format!("?{}", i + 2)).collect();
        let sql = format!(
            "UPDATE files SET status = 'missing', missing_since = ?1 WHERE id IN ({})",
            placeholders.join(",")
        );
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now.clone())];
        for id in chunk {
            params_vec.push(Box::new(id.to_string()));
        }
        let refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        marked += conn.execute(&sql, refs.as_slice())
            .map_err(storage_err("failed to mark missing"))? as u64;
    }

    Ok(marked)
}
```

- [ ] **Step 2: Update `table_row_counts`**

Replace `"file_history"` with `"file_transitions"` in the table list.

- [ ] **Step 3: Wire `purge_missing` into `db maintenance` command**

Find the db maintenance command and add a `purge_missing` call after vacuuming. Pass the retention config.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --workspace`
Expected: No warnings.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat: maintenance uses soft-delete and time-based purging"
```

---

### Task 10: Integration tests

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs` (or create new test file)

- [ ] **Step 1: Write end-to-end test for scan + reconciliation**

```rust
#[test]
fn scan_records_transitions_for_new_files() {
    // Create a temp dir with a media file
    // Run scan
    // Query file_transitions table
    // Assert one discovery transition exists
}
```

- [ ] **Step 2: Write test for external modification detection**

```rust
#[test]
fn scan_detects_external_modification() {
    // Create temp dir with media file, run scan
    // Modify the file content externally (change bytes, different hash)
    // Run scan again
    // Assert: old file marked missing, new file created, external transition recorded
}
```

- [ ] **Step 3: Write test for move detection**

```rust
#[test]
fn scan_detects_file_move() {
    // Create temp dir with media file, run scan
    // Rename the file
    // Run scan again
    // Assert: same UUID, path updated, discovery transition with detected_move
}
```

- [ ] **Step 4: Write test for process + transition recording**

```rust
#[test]
fn process_records_voom_transition() {
    // Create temp dir with media file, run scan
    // Run process with a policy that modifies the file
    // Assert: voom transition recorded with source_detail and plan_id
    // Assert: expected_hash updated to new hash
}
```

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo clippy --workspace && cargo fmt --all`
Expected: Clean.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "test: add integration tests for file lifecycle tracking"
```
