# Plan v2: Wire ingest_discovered_file into streaming pipeline (#377)

## Problem statement

`voom process` calls `cancel_scan_session` on its success path even though
the original design called for `finish_scan_session`. This workaround exists
because `finish_scan_session` computes its "missing files" set as the
difference between the `files` table and files registered against the
session via `ingest_discovered_file(session, df)` — but the streaming
pipeline never calls `ingest_discovered_file`. Sqlite-store's
`handle_file_discovered` calls only `upsert_discovered_file` (which writes
to a legacy `discovered_files` staging table that no production code path
reads from). If we called `finish` today, every pre-existing file would be
marked missing on every `voom process` run.

## Approach (Option C — direct call, mirrors `voom scan`)

Call `store.ingest_discovered_file(session, &df)` directly from the
streaming pipeline's ingest stage, exactly the way `crates/voom-cli/src/commands/scan/pipeline.rs:492`
already does it for `voom scan`. The bus event (`FileDiscoveredEvent`) is
still dispatched for observers (bus-tracer, event-log), but **persistence
no longer rides on the bus**.

### Why Option C and not Option A

The first revision of this plan proposed routing the session-registration
call through the existing `FileDiscoveredEvent` bus handler (Option A in
the issue). The Codex adversarial review pointed out three problems with
that approach, each of which Option C cleanly avoids:

1. **Fail-closed gap.** `dispatch_and_log` discards the
   `Vec<EventResult>` returned by the dispatcher. If sqlite-store's handler
   fails to register a file, the pipeline never sees the error and goes on
   to `finish_scan_session` — which would mark every unregistered file
   missing. Option A would have to add a new "fatal-on-PluginError" path
   through the dispatcher, which is more invasive than just calling the
   function directly.
2. **Move/ExternallyChanged invisibility.** The `IngestDecision` returned
   by `ingest_discovered_file` tells the caller "this file needs fresh
   introspection" via `needs_introspection_path`. Today's process worker
   uses `matches_discovery` (path/size/hash/tracks) as a cache check —
   which returns true for Moved files because the hash is the same. So if
   the bus handler swallows the decision, the worker silently skips
   re-introspection for Moved/ExternallyChanged files. Routing through the
   bus loses the per-decision return path. Option C keeps it.
3. **`--no-backup` reconciliation.** `voom scan` already handles the
   no-hash path via `mark_missing_paths`. Option A would have left the
   process pipeline cancelling the session on `--no-backup`, leaving users
   without missing-file reconciliation. Option C lets us reuse the same
   path-only reconciliation that scan uses.

Option B (kernel-maintained active-session register) was rejected from the
start as hidden global state.

## Affected files

| File | Change |
|------|--------|
| `crates/voom-cli/src/commands/process/pipeline_streaming.rs` | (1) Plumb `Arc<dyn StorageTrait>` and `ScanSessionId` into `spawn_ingest_stage` — `store` is already in the wider pipeline scope, just needs to be cloned in. (2) Collect a deduped `discovered_paths: Vec<PathBuf>` alongside `events_for_eta`. (3) For each `FileDiscoveredEvent`, when `event.content_hash.is_some()`, call `store.ingest_discovered_file(session, &df)`. On `Err`, cancel the pipeline token, cancel the session, and propagate. (4) Carry the `IngestDecision` forward by attaching a `needs_reintrospect: bool` field to `DiscoveredFilePayload` (derived from `decision.needs_introspection_path(&path).is_some()`). When the decision is `Unchanged`/`Duplicate`, set `needs_reintrospect = false`; otherwise true. (5) Return the deduped path set in `StreamingOutcome` so the success-path branching in `process::run` can call `mark_missing_paths`. |
| `crates/voom-cli/src/commands/process/mod.rs` | (1) Replace the unconditional success-path `cancel_scan_session` with: if cancelled → cancel; else if `args.no_backup` (no hashes available) → `store.mark_missing_paths(&discovered_paths, &paths)`; else → `store.finish_scan_session(session)`. Log outcomes (`missing`, `promoted_moves`). (2) On error path, keep cancel (already does this). (3) Add the new `discovered_paths` field on `StreamingOutcome` consumption. |
| `crates/voom-cli/src/introspect.rs` (where `DiscoveredFilePayload` lives — actually `voom-domain`) | Add `needs_reintrospect: bool` with `#[serde(default)]` defaulting to `true` (safer default; `false` is the optimization). `false` means "the row in `files` already matches; cache hit is safe". |
| `crates/voom-domain/src/...` | `DiscoveredFilePayload` lives here (`pub use voom_domain::DiscoveredFilePayload`); add the field as above. |
| `crates/voom-cli/src/commands/process/pipeline.rs::process_single_file` | When `payload.needs_reintrospect` is true, **bypass** `load_stored_file` / `matches_discovery` and go straight to `introspect_file`. When false, the existing cache-hit path runs unchanged. |
| `plugins/sqlite-store/src/lib.rs::handle_file_discovered` | **Unchanged.** The handler keeps writing to the legacy `discovered_files` staging table (no production reader, but harmless and preserves existing observers/tests). It does NOT call `ingest_discovered_file`. |
| `crates/voom-domain/src/events.rs::FileDiscoveredEvent` | **No change.** The session field is not needed because the pipeline handles registration directly. |
| `crates/voom-cli/tests/process_acceptance.rs` (new test) | (a) Populate a library, run `voom process`, assert no files are flagged `Missing`. (b) Delete one file, re-run `voom process`, assert that single file is flagged `Missing`. (c) Drop a file under a covered root that has a hash matching an existing row's `expected_hash` (synthetic move), assert `IngestDecision::Moved` triggers re-introspection (worker doesn't take the cache hit). (d) Run `voom process --no-backup` against a library where one file has been deleted; assert it's marked `Missing` via the `mark_missing_paths` path. |
| `plugins/sqlite-store/src/lib.rs` tests | Add a unit test simulating a sqlite-store handler failure during `FileDiscoveredEvent` dispatch — since the pipeline no longer depends on the handler for registration, this is informational only (the existing `discovered_files` write may fail but the run continues). Document this in the handler doc-comment. |

## Edge cases / risks

1. **Move / ExternallyChanged in the streaming pipeline.** Today the
   process worker uses `matches_discovery` as a cache check. After the
   switch, the row at the new (moved) path will match-by-hash and the
   worker would skip re-introspection. Fixed by threading a
   `needs_reintrospect` flag from `IngestDecision::needs_introspection_path`
   into `DiscoveredFilePayload`. Tested explicitly (see acceptance
   test (c)).

2. **`--no-backup` reconciliation.** With no hashes, the pipeline cannot
   call `ingest_discovered_file`. We collect the deduped discovered path
   set alongside `events_for_eta` and call `mark_missing_paths(&paths,
   &roots)` on the success path, mirroring `scan/pipeline.rs:404`.

3. **Failure of `ingest_discovered_file`.** Treated as fatal: cancel
   pipeline token (so siblings exit), call `cancel_scan_session` (so the
   session is marked cancelled and never finishes with partial data), and
   propagate the error.

4. **`ingest_discovered_file` is heavier than `upsert_discovered_file`.**
   It opens an immediate-mode transaction, does move-detection lookups,
   and writes the `files` table. Per-file cost on the ingest stage will
   go up. `voom scan` already runs the same workload synchronously and
   ships; the `plugins/sqlite-store/benches/scan_session.rs` benchmark
   covers it. No new benchmark needed for #377 specifically.

5. **`discovered_files` staging table.** Becomes essentially vestigial in
   the streaming flow (`upsert_discovered_file` still fires from the bus
   handler but no production read path consumes it). Flagging this as a
   follow-up dead-code candidate is reasonable; out of scope for #377.

6. **Existing 4 originally-regressing tests.** Must still pass:
   `test_plans::plans_show_after_processing`,
   `test_process_plan_only::process_skips_reintrospection_when_unchanged`,
   `test_workflow::full_lifecycle_dry_run`,
   `test_workflow::full_lifecycle_with_new_commands`. With Option C they
   should all pass because finish_scan_session will see all files
   registered.

7. **Cancellation interleavings.** Three cancellation surfaces in the
   streaming pipeline — outer token, pipeline_cancel (child token), pool
   cancel. The ingest_discovered_file call lives inside the
   tx_disc.recv() loop, between the `seen.insert` dedup check and the
   `tx_items.send`. On `Err` we cancel pipeline_cancel BEFORE returning
   so siblings observe cancellation. On `Ok` we proceed normally. After
   the loop, the existing `if !token.is_cancelled()` guards prevent the
   reporter from being seeded with a partial set when cancellation has
   already fired.

8. **Plumbing `store` into `spawn_ingest_stage`.** Already an
   `Arc<dyn StorageTrait>` in scope in `run_streaming_pipeline`. Adding
   it as a new parameter is mechanical.

9. **No event wire format change.** `FileDiscoveredEvent` is untouched.
   Existing WASM plugins and on-disk snapshots are unaffected.

## Test plan

- New acceptance tests (described above) in
  `crates/voom-cli/tests/process_acceptance.rs`.
- A direct unit test in `pipeline_streaming.rs` or a small integration
  test exercising the failure path: induce `ingest_discovered_file` to
  error on a particular file, assert (a) pipeline cancels, (b)
  cancel_scan_session is called, (c) `finish_scan_session` is NOT called.
- Verify the 4 originally-regressing tests pass.
- Verify the 7 existing acceptance tests pass.
- Add a unit test for `DiscoveredFilePayload` serde round-trip with the
  new field.

## Validation commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p voom-cli --features functional -- --test-threads=4
```

## Out of scope

- Restructuring the kernel event bus.
- Changes to fail-closed mutation recording or per-root gate model.
- Removing the legacy `discovered_files` staging table or the
  `upsert_discovered_file` call from sqlite-store's handler. (Tracked as
  potential follow-up dead-code work.)
- Re-architecting `matches_discovery` to consult IngestDecision directly
  — we use a per-payload bit instead, which is a smaller surface change.
