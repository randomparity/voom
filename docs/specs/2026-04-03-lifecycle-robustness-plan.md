# Lifecycle Robustness Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the file lifecycle tracking feature with a process lock, path normalization, and a dedicated recovery table.

**Architecture:** Four fixes applied to the CLI, discovery, domain, and sqlite-store crates. Fix 1 (process lock) and Fix 2 (path normalization) are independent. Fix 3 (recovery table) depends on both and includes rewriting crash recovery to query structured data instead of parsing event log summaries. Fix 4 (tests) covers all three.

**Tech Stack:** Rust, SQLite, `flock(2)` via `fs2` crate, `unicode-normalization` crate, `rusqlite`, clap.

---

### Task 1: Add `plan_id` field to `PlanExecutingEvent`

**Files:**
- Modify: `crates/voom-domain/src/events.rs` (lines 406-422)
- Modify: `crates/voom-cli/src/commands/process.rs` (lines 1211-1215)

- [ ] **Step 1: Write test for `PlanExecutingEvent` with `plan_id`**

In `crates/voom-domain/src/events.rs`, find the existing test around line 738 and update it:

```rust
        let plan_id = uuid::Uuid::new_v4();
        let event = Event::PlanExecuting(PlanExecutingEvent::new(
            plan_id,
            PathBuf::from("/test.mkv"),
            "normalize",
            3,
        ));
        assert_eq!(event.event_type(), "plan.executing");
        if let Event::PlanExecuting(e) = &event {
            assert_eq!(e.plan_id, plan_id);
        }
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cargo test -p voom-domain -- events::tests 2>&1 | tail -10
```

Expected: Compile error — `PlanExecutingEvent::new` doesn't accept `plan_id` yet.

- [ ] **Step 3: Add `plan_id` to `PlanExecutingEvent`**

In `crates/voom-domain/src/events.rs`, update the struct and constructor:

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanExecutingEvent {
    pub plan_id: Uuid,
    pub path: PathBuf,
    pub phase_name: String,
    pub action_count: usize,
}

impl PlanExecutingEvent {
    #[must_use]
    pub fn new(
        plan_id: Uuid,
        path: PathBuf,
        phase_name: impl Into<String>,
        action_count: usize,
    ) -> Self {
        Self {
            plan_id,
            path,
            phase_name: phase_name.into(),
            action_count,
        }
    }
}
```

- [ ] **Step 4: Fix the call site in `process.rs`**

In `crates/voom-cli/src/commands/process.rs`, update line 1211:

```rust
    let r = kernel.dispatch(Event::PlanExecuting(PlanExecutingEvent::new(
        plan.id,
        file.path.clone(),
        plan.phase_name.clone(),
        plan.actions.len(),
    )));
```

- [ ] **Step 5: Fix any other call sites**

Run:
```bash
cargo build --workspace 2>&1 | head -30
```

Search for any other `PlanExecutingEvent::new` calls that need updating. The mkvtoolnix-executor plugin may also construct this event. Fix all call sites to pass a plan UUID (use `plan.id` where available, or `Uuid::new_v4()` for synthetic events).

- [ ] **Step 6: Run tests to verify everything compiles and passes**

Run:
```bash
cargo test -p voom-domain -- events::tests && cargo test --workspace 2>&1 | tail -5
```

Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-domain/src/events.rs crates/voom-cli/src/commands/process.rs
# Also add any other files modified in step 5
git commit -m "feat: add plan_id to PlanExecutingEvent for recovery table"
```

---

### Task 2: Add process lock module

**Files:**
- Create: `crates/voom-cli/src/lock.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/Cargo.toml`

- [ ] **Step 1: Add `fs2` dependency**

In `crates/voom-cli/Cargo.toml`, add to `[dependencies]`:

```toml
fs2 = "0.4"
```

- [ ] **Step 2: Write unit tests for lock module**

Create `crates/voom-cli/src/lock.rs`:

```rust
//! File-based exclusive lock to prevent concurrent mutating VOOM operations.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use fs2::FileExt;

/// A held process lock. The lock is released when this is dropped.
pub struct ProcessLock {
    _file: File,
    path: PathBuf,
}

impl ProcessLock {
    /// Try to acquire an exclusive lock on `<data_dir>/voom.lock`.
    ///
    /// Returns `Ok(lock)` if acquired, or an error if another process holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("create data dir: {}", data_dir.display()))?;

        let lock_path = data_dir.join("voom.lock");
        let file = File::create(&lock_path)
            .with_context(|| format!("create lock file: {}", lock_path.display()))?;

        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self {
                _file: file,
                path: lock_path,
            }),
            Err(_) => {
                bail!(
                    "Another voom process is running (lock held on {}). \
                     Use --force to override.",
                    lock_path.display()
                );
            }
        }
    }
}

impl std::fmt::Debug for ProcessLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessLock")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_acquire_succeeds_on_fresh_dir() {
        let dir = tempdir().unwrap();
        let lock = ProcessLock::acquire(dir.path());
        assert!(lock.is_ok(), "should acquire lock on fresh dir");
    }

    #[test]
    fn test_acquire_fails_when_already_held() {
        let dir = tempdir().unwrap();
        let _lock1 = ProcessLock::acquire(dir.path()).unwrap();
        let lock2 = ProcessLock::acquire(dir.path());
        assert!(lock2.is_err(), "second acquire should fail");
        let err = lock2.unwrap_err().to_string();
        assert!(
            err.contains("Another voom process is running"),
            "error should mention another process: {err}"
        );
    }

    #[test]
    fn test_lock_released_on_drop() {
        let dir = tempdir().unwrap();
        {
            let _lock = ProcessLock::acquire(dir.path()).unwrap();
        }
        // Lock dropped — second acquire should succeed.
        let lock2 = ProcessLock::acquire(dir.path());
        assert!(lock2.is_ok(), "should acquire after first lock dropped");
    }

    #[test]
    fn test_creates_data_dir_if_missing() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("sub").join("dir");
        let lock = ProcessLock::acquire(&nested);
        assert!(lock.is_ok(), "should create nested dirs");
        assert!(nested.join("voom.lock").exists());
    }
}
```

- [ ] **Step 3: Run the lock tests**

Run:
```bash
cargo test -p voom-cli -- lock::tests 2>&1
```

Expected: All 4 tests pass.

- [ ] **Step 4: Add `--force` flag to CLI**

In `crates/voom-cli/src/cli.rs`, add to the `Cli` struct:

```rust
    /// Skip the process lock (use if a previous run crashed and left a stale lock)
    #[arg(long, global = true)]
    pub force: bool,
```

- [ ] **Step 5: Wire lock into `main.rs`**

In `crates/voom-cli/src/main.rs`, add `mod lock;` to the module declarations and update `main()`:

After the `let global_yes = cli.yes;` line (line 55), add:

```rust
    // Acquire exclusive lock for mutating commands, unless --force is set.
    let _lock = if !cli.force && command_needs_lock(&cli.command) {
        let config = config::load_config();
        Some(lock::ProcessLock::acquire(&config.data_dir)?)
    } else {
        None
    };
```

Add the helper function after `main()`:

```rust
/// Commands that write to the SQLite database require an exclusive lock.
fn command_needs_lock(cmd: &Commands) -> bool {
    matches!(
        cmd,
        Commands::Scan(_)
            | Commands::Process(_)
            | Commands::Serve(_)
            | Commands::Db(_)
    ) || matches!(cmd, Commands::Jobs(sub) if jobs_subcommand_mutates(sub))
}

fn jobs_subcommand_mutates(sub: &cli::JobsSubcommand) -> bool {
    matches!(
        sub,
        cli::JobsSubcommand::Cancel { .. }
            | cli::JobsSubcommand::Retry { .. }
            | cli::JobsSubcommand::Clear { .. }
    )
}
```

Note: `Commands::Db(_)` covers `db prune` and any future db subcommands. Check the actual `JobsSubcommand` enum variant names — adjust to match the real names in `cli.rs`.

- [ ] **Step 6: Verify it compiles and all tests pass**

Run:
```bash
cargo build -p voom-cli && cargo test -p voom-cli -- lock::tests 2>&1
```

Expected: Compiles, lock tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-cli/src/lock.rs crates/voom-cli/src/main.rs crates/voom-cli/src/cli.rs crates/voom-cli/Cargo.toml
git commit -m "feat: add flock-based process lock for mutating commands"
```

---

### Task 3: Add path normalization to discovery

**Files:**
- Modify: `plugins/discovery/src/scanner.rs`
- Modify: `plugins/discovery/Cargo.toml`

- [ ] **Step 1: Add `unicode-normalization` dependency**

In `plugins/discovery/Cargo.toml`, add to `[dependencies]`:

```toml
unicode-normalization = "0.1"
```

- [ ] **Step 2: Write tests for `normalize_path`**

In `plugins/discovery/src/scanner.rs`, add at the bottom inside or after the existing `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod normalize_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_normalize_path_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.mkv");
        std::fs::write(&file, b"data").unwrap();
        let result = normalize_path(&file);
        // Should be canonicalized (absolute, no symlinks)
        assert!(result.is_absolute());
        assert_eq!(result.file_name().unwrap(), "test.mkv");
    }

    #[test]
    fn test_normalize_path_missing_file_returns_raw() {
        let path = PathBuf::from("/nonexistent/file.mkv");
        let result = normalize_path(&path);
        assert_eq!(result, path, "missing file should return raw path");
    }

    #[test]
    fn test_normalize_path_nfc_normalization() {
        // é as NFD (e + combining acute) vs NFC (single codepoint)
        let nfd = "caf\u{0065}\u{0301}.mkv"; // e + combining accent
        let nfc = "caf\u{00e9}.mkv"; // single é
        let nfd_path = PathBuf::from(nfd);
        let result = normalize_path(&nfd_path);
        // The path string should be NFC-normalized
        let result_str = result.to_string_lossy();
        assert!(
            result_str.contains('\u{00e9}'),
            "should contain NFC é, got: {result_str}"
        );
        assert!(
            !result_str.contains('\u{0301}'),
            "should not contain combining accent after NFC"
        );
        // Both forms should normalize to the same string
        let nfc_result = normalize_path(&PathBuf::from(nfc));
        assert_eq!(
            result.to_string_lossy(),
            nfc_result.to_string_lossy(),
            "NFD and NFC inputs should produce identical normalized paths"
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run:
```bash
cargo test -p voom-discovery -- normalize_tests 2>&1 | tail -10
```

Expected: Compile error — `normalize_path` doesn't exist yet.

- [ ] **Step 4: Implement `normalize_path`**

In `plugins/discovery/src/scanner.rs`, add the function (and the import at the top of the file):

```rust
use unicode_normalization::UnicodeNormalization;

/// Normalize a file path for consistent storage and comparison.
///
/// Applies two transformations:
/// 1. `fs::canonicalize()` — resolves symlinks and macOS /var → /private/var
/// 2. Unicode NFC normalization — recomposes macOS NFD-decomposed filenames
///
/// Falls back to the raw path if canonicalization fails (e.g., file deleted
/// between walk and normalization).
pub fn normalize_path(path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let normalized: String = canonical.to_string_lossy().nfc().collect();
    PathBuf::from(normalized)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run:
```bash
cargo test -p voom-discovery -- normalize_tests 2>&1
```

Expected: All 3 tests pass.

- [ ] **Step 6: Apply normalization in the discovery scan loop**

In `plugins/discovery/src/scanner.rs`, in the `scan_directory` function, find the line where paths are collected (around line 203):

```rust
            media_paths.push((path, size));
```

Replace with:

```rust
            media_paths.push((normalize_path(&path), size));
```

- [ ] **Step 7: Export `normalize_path` from the crate**

In `plugins/discovery/src/lib.rs`, add to the public re-exports:

```rust
pub use scanner::normalize_path;
```

(Check the existing `pub use` statements and follow the same pattern.)

- [ ] **Step 8: Verify workspace builds and tests pass**

Run:
```bash
cargo build --workspace && cargo test -p voom-discovery 2>&1 | tail -10
```

Expected: Builds and all tests pass.

- [ ] **Step 9: Commit**

```bash
git add plugins/discovery/src/scanner.rs plugins/discovery/src/lib.rs plugins/discovery/Cargo.toml
git commit -m "feat: normalize discovered paths with canonicalize + NFC"
```

---

### Task 4: Add `pending_operations` table and storage trait

**Files:**
- Modify: `plugins/sqlite-store/src/schema.rs`
- Modify: `crates/voom-domain/src/storage.rs`
- Create: `plugins/sqlite-store/src/store/pending_ops_storage.rs`
- Modify: `plugins/sqlite-store/src/store/mod.rs`

- [ ] **Step 1: Define `PendingOperation` struct and `PendingOpsStorage` trait**

In `crates/voom-domain/src/storage.rs`, add the struct and trait before the `StorageTrait` definition:

```rust
/// A record of an in-flight plan execution, used for crash recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOperation {
    /// Plan UUID.
    pub id: Uuid,
    /// Normalized path of the file being processed.
    pub file_path: PathBuf,
    /// Name of the phase being executed.
    pub phase_name: String,
    /// When execution started.
    pub started_at: DateTime<Utc>,
}

/// Storage for tracking in-flight operations (crash recovery).
pub trait PendingOpsStorage {
    /// Record that a plan is now executing.
    fn insert_pending_op(&self, op: &PendingOperation) -> Result<()>;

    /// Remove a completed or failed operation.
    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()>;

    /// List all pending operations (orphans after a crash).
    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>>;
}
```

Add `PendingOpsStorage` to the `StorageTrait` supertrait list and the blanket impl:

```rust
pub trait StorageTrait:
    FileStorage
    + JobStorage
    + PlanStorage
    + FileTransitionStorage
    + StatsStorage
    + PluginDataStorage
    + BadFileStorage
    + MaintenanceStorage
    + HealthCheckStorage
    + EventLogStorage
    + SnapshotStorage
    + PendingOpsStorage
{
}

impl<T> StorageTrait for T where
    T: FileStorage
        + JobStorage
        + PlanStorage
        + FileTransitionStorage
        + StatsStorage
        + PluginDataStorage
        + BadFileStorage
        + MaintenanceStorage
        + HealthCheckStorage
        + EventLogStorage
        + SnapshotStorage
        + PendingOpsStorage
{
}
```

- [ ] **Step 2: Add the table to the schema SQL**

In `plugins/sqlite-store/src/schema.rs`, add inside the `SCHEMA_SQL` string, after the `library_snapshots` table (before the closing `"#`):

```sql
CREATE TABLE IF NOT EXISTS pending_operations (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    started_at TEXT NOT NULL
);
```

Also add `"pending_operations"` to the `KNOWN_TABLES` array in the `migrate()` function.

- [ ] **Step 3: Add migration for existing databases**

In `plugins/sqlite-store/src/schema.rs`, add to the `migrate()` function (following the existing pattern):

```rust
    let has_pending_ops: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='pending_operations'",
        [],
        |row| row.get(0),
    )?;
    if !has_pending_ops {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_operations (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                phase_name TEXT NOT NULL,
                started_at TEXT NOT NULL
            );",
        )?;
    }
```

- [ ] **Step 4: Implement SQLite storage for `pending_operations`**

Create `plugins/sqlite-store/src/store/pending_ops_storage.rs`:

```rust
use anyhow::Result;
use rusqlite::params;
use uuid::Uuid;

use voom_domain::storage::PendingOperation;

use super::{format_datetime, storage_err, SqliteStore};

impl voom_domain::storage::PendingOpsStorage for SqliteStore {
    fn insert_pending_op(&self, op: &PendingOperation) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO pending_operations \
             (id, file_path, phase_name, started_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                op.id.to_string(),
                op.file_path.to_string_lossy().to_string(),
                op.phase_name,
                format_datetime(&op.started_at),
            ],
        )
        .map_err(storage_err("failed to insert pending operation"))?;
        Ok(())
    }

    fn delete_pending_op(&self, plan_id: &Uuid) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM pending_operations WHERE id = ?1",
            params![plan_id.to_string()],
        )
        .map_err(storage_err("failed to delete pending operation"))?;
        Ok(())
    }

    fn list_pending_ops(&self) -> Result<Vec<PendingOperation>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, file_path, phase_name, started_at \
                 FROM pending_operations ORDER BY started_at",
            )
            .map_err(storage_err("failed to prepare pending ops query"))?;

        let ops = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let file_path: String = row.get(1)?;
                let phase_name: String = row.get(2)?;
                let started_at_str: String = row.get(3)?;

                Ok((id_str, file_path, phase_name, started_at_str))
            })
            .map_err(storage_err("failed to query pending ops"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage_err("failed to collect pending ops"))?;

        let mut result = Vec::with_capacity(ops.len());
        for (id_str, file_path, phase_name, started_at_str) in ops {
            let id = super::parse_uuid(&id_str)?;
            let started_at = started_at_str.parse().map_err(|e| {
                anyhow::anyhow!("corrupt datetime in pending_operations: {started_at_str}: {e}")
            })?;
            result.push(PendingOperation {
                id,
                file_path: std::path::PathBuf::from(file_path),
                phase_name,
                started_at,
            });
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::test_store;
    use chrono::Utc;
    use voom_domain::storage::PendingOpsStorage;

    #[test]
    fn test_insert_and_list() {
        let store = test_store();
        let op = PendingOperation {
            id: Uuid::new_v4(),
            file_path: "/movies/test.mkv".into(),
            phase_name: "normalize".into(),
            started_at: Utc::now(),
        };
        store.insert_pending_op(&op).unwrap();
        let ops = store.list_pending_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].id, op.id);
        assert_eq!(ops[0].phase_name, "normalize");
    }

    #[test]
    fn test_delete_removes_op() {
        let store = test_store();
        let op = PendingOperation {
            id: Uuid::new_v4(),
            file_path: "/movies/test.mkv".into(),
            phase_name: "normalize".into(),
            started_at: Utc::now(),
        };
        store.insert_pending_op(&op).unwrap();
        store.delete_pending_op(&op.id).unwrap();
        let ops = store.list_pending_ops().unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_list_empty() {
        let store = test_store();
        let ops = store.list_pending_ops().unwrap();
        assert!(ops.is_empty());
    }
}
```

- [ ] **Step 5: Register the module**

In `plugins/sqlite-store/src/store/mod.rs`, add:

```rust
mod pending_ops_storage;
```

- [ ] **Step 6: Implement `PendingOpsStorage` on `InMemoryStore`**

In `crates/voom-domain/src/test_support.rs`, implement the trait for `InMemoryStore` so unit tests elsewhere can use it. Add a `pending_ops: Mutex<Vec<PendingOperation>>` field and implement all three methods. Follow the pattern of the other in-memory trait implementations in that file.

- [ ] **Step 7: Verify everything compiles and tests pass**

Run:
```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: All tests pass including the new pending_ops tests.

- [ ] **Step 8: Commit**

```bash
git add crates/voom-domain/src/storage.rs crates/voom-domain/src/test_support.rs \
  plugins/sqlite-store/src/schema.rs plugins/sqlite-store/src/store/mod.rs \
  plugins/sqlite-store/src/store/pending_ops_storage.rs
git commit -m "feat: add pending_operations table and PendingOpsStorage trait"
```

---

### Task 5: Wire `pending_operations` into the event pipeline

**Files:**
- Modify: `plugins/sqlite-store/src/lib.rs` (lines 70-234)

- [ ] **Step 1: Add insert on `PlanExecuting` event**

In `plugins/sqlite-store/src/lib.rs`, inside the `on_event` match block, add a handler for `PlanExecuting` before the existing `PlanCreated` handler (around line 103):

```rust
            Event::PlanExecuting(e) => {
                let op = voom_domain::storage::PendingOperation {
                    id: e.plan_id,
                    file_path: e.path.clone(),
                    phase_name: e.phase_name.clone(),
                    started_at: chrono::Utc::now(),
                };
                if let Err(err) = store.insert_pending_op(&op) {
                    tracing::warn!(
                        error = %err,
                        plan_id = %e.plan_id,
                        "failed to insert pending operation"
                    );
                }
            }
```

- [ ] **Step 2: Add delete on `PlanCompleted` event**

In the existing `Event::PlanCompleted(e)` handler (around line 108), add after the `update_plan_status` call:

```rust
                if let Err(err) = store.delete_pending_op(&e.plan_id) {
                    tracing::warn!(
                        error = %err,
                        plan_id = %e.plan_id,
                        "failed to delete pending operation on completion"
                    );
                }
```

- [ ] **Step 3: Add delete on `PlanFailed` event**

In the existing `Event::PlanFailed(e)` handler (around line 122), add after the `update_plan_status` call:

```rust
                if let Err(err) = store.delete_pending_op(&e.plan_id) {
                    tracing::warn!(
                        error = %err,
                        plan_id = %e.plan_id,
                        "failed to delete pending operation on failure"
                    );
                }
```

- [ ] **Step 4: Verify it compiles**

Run:
```bash
cargo build -p sqlite-store 2>&1 | tail -10
```

Expected: Compiles cleanly.

- [ ] **Step 5: Commit**

```bash
git add plugins/sqlite-store/src/lib.rs
git commit -m "feat: write to pending_operations on PlanExecuting/Completed/Failed"
```

---

### Task 6: Rewrite crash recovery to use `pending_operations`

**Files:**
- Modify: `crates/voom-cli/src/recovery.rs`

- [ ] **Step 1: Update `is_crash_orphan` and `check_and_recover_under`**

Replace the `is_crash_orphan` function and update `check_and_recover_under` to use `list_pending_ops` instead of event log queries.

The new `check_and_recover_under`:

```rust
pub fn check_and_recover_under(
    config: &RecoveryConfig,
    scan_dirs: &[PathBuf],
    store: &dyn voom_domain::storage::StorageTrait,
) -> Result<u64> {
    // Step 1: Check the pending_operations table for crash orphans.
    let pending = store.list_pending_ops().unwrap_or_default();

    // Step 2: Find .vbak files on disk.
    let all_backups = find_orphans_under(scan_dirs)?;

    if pending.is_empty() && all_backups.is_empty() {
        return Ok(0);
    }

    // Build a set of file paths with pending operations for cross-reference.
    let pending_paths: std::collections::HashSet<String> = pending
        .iter()
        .map(|op| op.file_path.to_string_lossy().to_string())
        .collect();

    // An orphan is a backup whose original file has a pending operation,
    // OR any backup found when pending operations exist for that path.
    let orphans: Vec<_> = all_backups
        .into_iter()
        .filter(|b| {
            let path_str = b.original_path.to_string_lossy().to_string();
            pending_paths.contains(&path_str)
        })
        .collect();

    if orphans.is_empty() {
        if !pending.is_empty() {
            // Pending ops exist but no backups found — clean up stale ops.
            for op in &pending {
                tracing::warn!(
                    plan_id = %op.id,
                    path = %op.file_path.display(),
                    "stale pending operation with no backup — removing"
                );
                let _ = store.delete_pending_op(&op.id);
            }
        }
        return Ok(0);
    }

    tracing::info!(
        count = orphans.len(),
        "found orphaned backup files from crashed executions"
    );

    let mut resolved = 0u64;
    for orphan in &orphans {
        let result = match config.mode.as_str() {
            "always_recover" => recover(orphan, store),
            "always_discard" => discard(orphan, store),
            _ => {
                tracing::warn!(
                    backup = %orphan.backup_path.display(),
                    "orphaned backup found — set recovery.mode in config.toml"
                );
                continue;
            }
        };
        match result {
            Ok(()) => {
                resolved += 1;
                // Clean up the pending operation row.
                let path_str = orphan.original_path.to_string_lossy().to_string();
                for op in pending.iter().filter(|op| {
                    op.file_path.to_string_lossy().to_string() == path_str
                }) {
                    let _ = store.delete_pending_op(&op.id);
                }
            }
            Err(e) => tracing::warn!(
                backup = %orphan.backup_path.display(),
                error = %e,
                "failed to resolve orphaned backup"
            ),
        }
    }
    Ok(resolved)
}
```

- [ ] **Step 2: Remove `is_crash_orphan` function**

Delete the `is_crash_orphan` function entirely (lines 100-148). It is no longer called.

- [ ] **Step 3: Apply path normalization in `infer_original_path`**

Update `infer_original_path` to normalize the inferred path:

```rust
fn infer_original_path(backup_path: &Path) -> Option<PathBuf> {
    let backup_dir = backup_path.parent()?;
    let original_dir = backup_dir.parent()?;
    let backup_filename = backup_path.file_name()?.to_string_lossy();
    let without_ext = backup_filename.strip_suffix(".vbak")?;
    let original_filename = strip_timestamp_suffix(without_ext)?;
    let raw_path = original_dir.join(original_filename);
    Some(voom_discovery::normalize_path(&raw_path))
}
```

- [ ] **Step 4: Update unit tests**

Update the test helpers `insert_executing_event` and `insert_completed_event` to also write to `pending_operations`. Update `is_crash_orphan` tests to test the new behavior (pending_ops table presence instead of event log parsing).

Replace the `is_crash_orphan` tests and `check_and_recover_under` tests:

- Tests that previously called `insert_executing_event` should now also call `store.insert_pending_op(...)`.
- Tests that previously called `insert_completed_event` (to mark as not-orphan) should now also call `store.delete_pending_op(...)`.
- The `test_check_and_recover_skips_backup_with_no_events` test becomes `test_check_and_recover_skips_backup_with_no_pending_ops` — a backup with no pending operation row should be left alone.

- [ ] **Step 5: Verify all recovery tests pass**

Run:
```bash
cargo test -p voom-cli -- recovery::tests 2>&1
```

Expected: All tests pass with the new pending_operations-based logic.

- [ ] **Step 6: Verify the full workspace compiles and tests pass**

Run:
```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-cli/src/recovery.rs
git commit -m "feat: rewrite crash recovery to use pending_operations table"
```

---

### Task 7: Add functional tests

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs`

- [ ] **Step 1: Add lock contention test**

Inside the `test_lifecycle_advanced` module, add:

```rust
    #[test]
    fn lock_prevents_concurrent_scan() {
        require_tool!("ffprobe");
        let env = TestEnv::new();
        let roots = populate_single_root(&env, 2);

        // Hold the lock by acquiring it on the voom data dir.
        let lock_path = env.voom_dir.join("voom.lock");
        std::fs::create_dir_all(&env.voom_dir).unwrap();
        let lock_file = std::fs::File::create(&lock_path).unwrap();
        use fs2::FileExt;
        lock_file.lock_exclusive().unwrap();

        // Second voom scan should fail with lock error.
        let output = env
            .voom()
            .args(["scan", roots[0].to_str().unwrap()])
            .timeout(std::time::Duration::from_secs(10))
            .assert()
            .failure();

        let stderr = String::from_utf8_lossy(&output.get_output().stderr);
        assert!(
            stderr.contains("Another voom process is running"),
            "should mention lock contention: {stderr}"
        );

        // --force should bypass the lock.
        lock_file.unlock().unwrap();
        lock_file.lock_exclusive().unwrap();
        env.voom()
            .args(["--force", "scan", roots[0].to_str().unwrap()])
            .timeout(std::time::Duration::from_secs(60))
            .assert()
            .success();
    }
```

Note: Adjust `populate_single_root` to match the actual test helper. You may need to use `populate_multi_root(&env, 1, 2)` and take `roots[0]`.

- [ ] **Step 2: Update existing crash recovery tests**

Update the three existing crash recovery tests (`crash_recovery_always_recover`, `crash_recovery_always_discard`, `normal_backup_not_treated_as_orphan`) to insert into `pending_operations` instead of (or in addition to) `event_log`.

For crash orphan tests, insert a `pending_operations` row via direct SQL:

```rust
    fn insert_pending_op(db: &std::path::Path, plan_id: &str, file_path: &str, phase: &str) {
        let conn = rusqlite::Connection::open(db).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO pending_operations (id, file_path, phase_name, started_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![plan_id, file_path, phase, now],
        )
        .unwrap();
    }
```

For the `normal_backup_not_treated_as_orphan` test, ensure there is NO `pending_operations` row (because a completed execution would have deleted it).

- [ ] **Step 3: Add recovery-survives-pruning test**

```rust
    #[test]
    fn recovery_survives_event_log_pruning() {
        require_tool!("ffprobe");
        let env = TestEnv::new();
        populate_media_files(&env, &["basic-h264-aac"]);

        // Scan to bootstrap the database.
        env.voom()
            .args(["scan", env.media_dir().to_str().unwrap()])
            .timeout(std::time::Duration::from_secs(60))
            .assert()
            .success();

        // Create orphaned backup manually.
        let original = env.media_dir().join("basic-h264-aac.mp4");
        let canon_original = std::fs::canonicalize(&original).unwrap();
        let backup_dir = env.media_dir().join(".voom-backup");
        std::fs::create_dir_all(&backup_dir).unwrap();
        let vbak_name = format!(
            "{}.20260403120000.vbak",
            original.file_name().unwrap().to_string_lossy()
        );
        let vbak_path = backup_dir.join(&vbak_name);
        std::fs::copy(&original, &vbak_path).unwrap();

        // Insert pending operation.
        let plan_id = uuid::Uuid::new_v4().to_string();
        insert_pending_op(
            &env.db_path(),
            &plan_id,
            &canon_original.to_string_lossy(),
            "normalize",
        );

        // Insert a lot of events to trigger pruning (> 10K).
        // The pending_operations row should survive.
        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        for i in 0..11_000 {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO event_log (id, event_type, payload, summary, created_at) \
                 VALUES (?1, 'test.bulk', '{}', ?2, datetime('now'))",
                rusqlite::params![id, format!("bulk event {i}")],
            )
            .unwrap();
        }
        drop(conn);

        // Set recovery mode and process — should find the orphan.
        set_recovery_mode(&env, "always_recover");
        let policy = env.write_policy("test", TEST_POLICY);
        env.voom()
            .args([
                "process",
                env.media_dir().to_str().unwrap(),
                "--policy",
                policy.to_str().unwrap(),
            ])
            .timeout(std::time::Duration::from_secs(120))
            .assert()
            .success();

        // Backup should be cleaned up.
        assert!(!vbak_path.exists(), "orphan backup should be resolved");

        // Pending op should be cleaned up.
        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_operations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "pending_operations should be empty after recovery");
    }
```

- [ ] **Step 4: Run all functional tests**

Run:
```bash
cargo test -p voom-cli --features functional -- test_lifecycle_advanced --test-threads=2 2>&1 | tail -20
```

Expected: All tests pass including new and updated ones.

- [ ] **Step 5: Run the full test suite for regressions**

Run:
```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-cli/tests/functional_tests.rs crates/voom-cli/Cargo.toml
git commit -m "test: add functional tests for lock, recovery table, and pruning survival"
```

---

### Task 8: Final verification and cleanup

**Files:**
- No new files.

- [ ] **Step 1: Run clippy**

```bash
cargo clippy --workspace 2>&1 | tail -20
```

Fix any warnings.

- [ ] **Step 2: Run fmt**

```bash
cargo fmt --all
```

- [ ] **Step 3: Run the full test suite one more time**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: All tests pass, no warnings.

- [ ] **Step 4: Commit any cleanup**

```bash
git add -u
git commit -m "chore: clippy and fmt cleanup for robustness fixes"
```
