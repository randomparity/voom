# Plan — Issue #151: Skip re-introspection of unchanged files in `voom process`

GitHub issue: <https://github.com/randomparity/voom/issues/151>
Target branch: `fix/issue-151-skip-reintrospection` off `main` (32228b4).

## Problem (one-line)

`voom process` re-runs `ffprobe` on every already-introspected file, costing
~12 minutes per pass on a 6,774-file library and inflating the `jobs` and
`file_introspected` event log by one row per file per run.

## Root cause

`crates/voom-cli/src/commands/process/pipeline.rs:63` always calls
`crate::introspect::introspect_file(...)` for every job, regardless of
whether the `files` table already holds a fresh `MediaFile` for the same
`(path, size, content_hash)`. The discovery layer already has a stored-
fingerprint short-circuit (`reuse_cached_hash` in
`plugins/discovery/src/scanner.rs:345`), but no equivalent exists for
introspection.

Secondary: the `ffprobe-introspector` plugin emits a `JobEnqueueRequested`
on every `FileDiscovered`
(`plugins/ffprobe-introspector/src/lib.rs:117`). No consumer claims those
`JobType::Introspect` rows, so they accumulate in the `jobs` table.

## Fix strategy

Mirror the discovery short-circuit one layer up:

1. Before invoking ffprobe in `process_single_file`, look up the stored
   `MediaFile` and use it directly when the discovered file's `size` and
   `content_hash` match the stored values and the stored row is `Active`.
2. Bypass the cache when `--force-rescan` is set.
3. Stop emitting unconsumed `JobType::Introspect` rows (lightweight cleanup
   that follows from the above).

Out of scope: re-architecting the ffprobe-introspector plugin to take a
storage handle, changing scan-side behavior, persisting `mtime` in the
`files` schema. The existing `(size, content_hash)` pair is sufficient and
matches what discovery already uses.

## File-level changes

### 1. `crates/voom-cli/src/introspect.rs`
Add a helper that returns the stored `MediaFile` when it matches the
discovery payload:

```rust
pub async fn try_load_cached_file(
    store: Arc<dyn StorageTrait>,
    path: &Path,
    discovered_size: u64,
    discovered_hash: Option<&str>,
) -> Option<MediaFile>;
```

Match rules (all must hold; otherwise return `None`):

- `store.file_by_path(path)` returns `Some(stored)`.
- `stored.status == FileStatus::Active`.
- `stored.size == discovered_size`.
- `discovered_hash` is `Some` AND `stored.content_hash == discovered_hash`
  (if either side is `None`, fall through to ffprobe — preserves the
  `--no-backup` path where hashing is off).
- `stored.tracks` is non-empty (guards against partially-populated rows).

Runs the synchronous `file_by_path` lookup on `spawn_blocking`, mirroring
the existing pattern at `pipeline.rs:78`.

### 2. `crates/voom-cli/src/commands/process/pipeline.rs`
Replace the unconditional `introspect_file` call at line 63 with:

```rust
let cached = if ctx.force_rescan {
    None
} else {
    crate::introspect::try_load_cached_file(
        ctx.store.clone(),
        &path,
        payload.size,
        payload.content_hash.as_deref(),
    ).await
};

let mut file = match cached {
    Some(f) => {
        tracing::debug!(path = %path.display(), "skipped ffprobe (cache hit)");
        f
    }
    None => crate::introspect::introspect_file(
        path,
        payload.size,
        payload.content_hash,
        &ctx.kernel,
        ctx.ffprobe_path,
    ).await.map_err(...)?,
};
```

Notes:

- The `plugin_metadata` merge block (lines 73–94) becomes redundant on the
  cache-hit path because `stored.plugin_metadata` is already in `file`.
  Move the merge inside the `None` arm to avoid a second `file_by_path`.
- `apply_detected_languages(&mut file)` still runs after either path so
  the evaluator sees normalized track languages.
- The TOCTOU hash check in `check_file_hash` at line 471 already runs
  before execution, so cache hits remain safe under concurrent writes.

### 3. `crates/voom-cli/src/commands/process/mod.rs`
Thread `force_rescan` into `ProcessContext`:

```rust
pub(super) struct ProcessContext<'a> {
    ...
    pub(super) force_rescan: bool,
    ...
}
```

Set from `args.force_rescan` at the call site (`mod.rs:224`).

### 4. `crates/voom-cli/src/cli.rs:188`
Update the `--force-rescan` doc comment to reflect the broader meaning:

```text
/// Re-attempt introspection from scratch. Without this flag, files already
/// fully introspected are reused from the database and known-bad files are
/// skipped.
```

### 5. `plugins/ffprobe-introspector/src/lib.rs`
Stop emitting `JobEnqueueRequested` for introspection. The job has no
consumer (verified: only `crates/voom-cli/src/introspect.rs` and tests
reference `JobType::Introspect`); the CLI drives introspection directly.
The `on_event` handler becomes a no-op for `FileDiscovered`. Keep the
event subscription removed to avoid event-bus latency for an event the
plugin no longer acts on. Update the four affected unit tests in the same
file (`test_handles_file_discovered`, `test_on_event_produces_enqueue_event`,
plus the matching kernel-registration test).

This is a clean removal — no shim, no flag — per the project's
"replace, don't deprecate" rule.

## Tests

### Unit (`crates/voom-cli/src/introspect.rs`)
- `try_load_cached_file` returns `Some` for an exact (size, hash) match
  on an Active row with tracks.
- Returns `None` when stored size differs.
- Returns `None` when stored hash differs.
- Returns `None` when `discovered_hash` is `None` (no-hash mode).
- Returns `None` when the row exists but `status == Missing`.
- Returns `None` when stored tracks vec is empty.

Use the in-memory `SqliteStore::open(":memory:")` via the existing
`tests/common` harness pattern.

### Functional (`crates/voom-cli/tests/functional_tests.rs`)
Add a test that:

1. Runs `voom scan -r --no-hash <fixture-dir>` (or the hashing variant).
2. Counts `file.introspected` events emitted during the scan.
3. Runs `voom process --plan-only <fixture-dir>` against the same DB.
4. Asserts zero new `file.introspected` events were emitted on pass 2.
5. Runs `voom process --force-rescan --plan-only <fixture-dir>` and
   asserts events are emitted again.

Use the existing fixture under `crates/voom-cli/tests/fixtures/` (already
exercises the policy pipeline). Because functional tests already shell
out to ffprobe via the test harness, an introspection-skip win is
observable via event counts rather than wall time.

### Plugin unit tests
Update `plugins/ffprobe-introspector/src/lib.rs` tests to reflect the
removed `JobEnqueueRequested` emission. Drop assertions on
`result.produced_events`; replace with an assertion that `on_event`
returns `Ok(None)` for `FileDiscovered`.

## Risk and mitigations

| Risk | Mitigation |
|---|---|
| Stale cached `MediaFile` slips past the (size, hash) gate after a manual edit that preserves both. | The TOCTOU check in `check_file_hash` (`pipeline.rs:471`) re-hashes before execution; mismatch → skip with `"file changed since introspection"`. |
| `--no-backup` path skips hashing, so `discovered_hash` is `None` and the cache never hits. | Acceptable — this is the same trade-off the discovery short-circuit makes. Documented in the helper's rustdoc. |
| Plugin removal breaks downstream WASM plugins that subscribe to `JobEnqueueRequested` from this source. | None today: capability matrix shows ffprobe-introspector is the only emitter for `JobType::Introspect`, and no plugin claims it. Verified via `rg "JobType::Introspect"` (10 hits, all in tests / definitions / docs). |
| `try_load_cached_file` adds a `file_by_path` per file. | sqlite-store already has `idx_files_path` (`schema.rs:165`); the lookup is one indexed point query per file — comparable to the existing fingerprint lookup discovery already does. |

## Verification

Local:
```bash
cargo test -p voom-cli
cargo test -p voom-ffprobe-introspector
cargo test -p voom-cli --features functional -- --test-threads=4
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Manual on a real library:
1. `voom scan -r <dir>`
2. `time voom process --plan-only <dir>` — should be near-instant for
   already-introspected files; previously 12 min on a 6,774-file library.
3. `voom process --force-rescan --plan-only <dir>` — should re-introspect
   (back to previous timing).
4. `voom events list --kind file.introspected | wc -l` — should not grow
   between runs unless `--force-rescan` is used.

## Commit plan

One commit:

```
fix(process): skip re-introspection for unchanged files (closes #151)
```

Includes: helper, pipeline change, ProcessContext field, CLI doc update,
ffprobe-introspector enqueue removal, unit tests, functional test.
