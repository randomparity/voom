# Open Issue Triage & Implementation Plans — 2026-04-18

Triage of the 25 open GitHub issues at time of review, with per-issue
relevance assessment and implementation plans for the still-relevant ones.

Verified against branch `desloppify/code-health` @ 18df0e6.

## Summary

| # | Title | Status | Action |
|---|-------|--------|--------|
| 142 | ContainerIncompatible safeguard: extend check to synthesized tracks | **Relevant** | Implement |
| 131 | Need Better Process to Clean Backup Directories | **Relevant** | Design + implement |
| 94 | Improve *arr Stack Integration | **Relevant (design)** | Defer — subsumed by #37 |
| 93 | Applying Policy Forces Full Hash on Library | **Relevant** | Implement |
| 92 | Support Plugin Stats | **Relevant (design)** | Scope as sprint-sized feature |
| 91 | Multi-Host Agent Coordination | **Relevant (design)** | Keep open — large future effort |
| 90 | Consider Container Release Options | **Relevant (ops)** | Implement |
| 47 | Create UNIX Compliant Output for Scripting | **Relevant** | Implement |
| 44 | Verify tower middleware layer ordering for rate limiting | **Resolved, no change** | Close |
| 43 | Audit StorageReporter for async safety in daemon mode | **Relevant (docs-only)** | Implement |
| 42 | Use init_and_register for capability-collector plugin | **Relevant** | Implement |
| 41 | Add daemon_mode parameter to bootstrap_kernel_with_store | **Outdated** | Close |
| 40 | Executors: use probed capabilities in can_handle() | **Done** | Close |
| 39 | Policy evaluator: subscribe to executor capability events | **Done (via collector)** | Close |
| 38 | Plugin: executor capability announcement via init-time probing | **Done** | Close |
| 37 | Plugin: notification/webhook forwarding | **Relevant (design)** | New plugin |
| 36 | Plugin: filesystem watcher | **Relevant (design)** | New plugin — feeds #4 |
| 35 | Plugin: library-index emitting stats from SQLite at startup | **Relevant (design)** | New plugin |
| 34 | Plugin: health-check emitting system readiness events | **Done** | Close |
| 33 | Plugin: config/policy validator emitting init-time events | **Relevant (design)** | New plugin |
| 31 | No per-IP rate limiting on web API | **Relevant** | Implement |
| 30 | No minimum entropy check on web auth token | **Relevant** | Implement |
| 28 | CSP trusts unpkg.com without SRI hashes | **Relevant** | Implement |
| 27 | Event bus: FileDiscovered dispatched with zero subscribers | **Outdated** | Close |
| 4  | Event-driven pipeline under serve with periodic scanning | **Relevant (sprint-sized)** | Plan as sprint |

**Close immediately (6):** #27, #34, #38, #39, #40, #41, #44 (7 total — #44 explicitly says "no code change needed").

**Small-to-medium fixes (8):** #28, #30, #31, #42, #43, #47, #93, #142.

**New feature work (5):** #33, #35, #36, #37, #131.

**Sprint-sized / design (5):** #4, #90, #91, #92, #94.

---

## Close-on-Review (verification evidence)

### #27 — FileDiscovered has zero subscribers

**Current state:** `plugins/sqlite-store/src/lib.rs:79` — `Event::FileDiscovered(e) => handle_file_discovered(store, e)?`. Test at line 529 asserts the same. `handles()` at line 395 returns true for `FILE_DISCOVERED`.

**Action:** Close with comment citing `plugins/sqlite-store/src/lib.rs:79` and `plugins/sqlite-store/src/lib.rs:395`.

### #34 — health-check plugin

**Current state:** `plugins/health-checker/` crate exists with `HealthCheckerPlugin`, `HealthStatusEvent`, and config. Registered as a native plugin.

**Action:** Close with comment citing `plugins/health-checker/src/lib.rs`.

### #38 / #39 / #40 — Executor capability events

**Current state:**

- `plugins/ffmpeg-executor/src/lib.rs:628` emits `ExecutorCapabilitiesEvent::new("ffmpeg-executor", …)` from `init()`.
- `plugins/mkvtoolnix-executor/src/lib.rs:456` emits `ExecutorCapabilitiesEvent::new("mkvtoolnix-executor", …)` from `init()`.
- `plugins/capability-collector` subscribes and provides a snapshot.
- `plugins/policy-evaluator/src/evaluator.rs:1092` uses the snapshot to validate plans.
- `plugins/ffmpeg-executor/src/lib.rs:176` — `can_handle_probed` checks `probed_codecs.encoders` against the target codec.

**Action:** Close all three with cross-references to the evidence above.

### #41 — daemon_mode parameter for bootstrap

**Current state:** `crates/voom-cli/src/app.rs:268` — `job_queue` is registered as a resource only on the `job-manager`'s `PluginContext`, not on `ffprobe-introspector`. `plugins/ffprobe-introspector/src/` has no references to `job_queue`. The reported problem no longer exists.

**Action:** Close with a pointer to `crates/voom-cli/src/app.rs:264-276`.

### #44 — Tower layer ordering

**Current state:** Issue text itself concludes "No code change needed". This is a stale documentation-only issue.

**Action:** Close as resolved — the documentation lives in the issue itself.

---

## Small-to-medium implementation plans

### #142 — Extend ContainerIncompatible safeguard to synthesized tracks

**Scope:** ~40 lines + test.

**Files:**

- `plugins/policy-evaluator/src/evaluator.rs` — modify `apply_container_safeguard` (lines 300–379).
- `plugins/policy-evaluator/tests/` — new test case (MKV → WebM + synthesize AAC → violation; MKV → WebM + synthesize Opus → no violation).

**Plan:**

1. After the existing loop over `file.tracks` (evaluator.rs:339–350), add a loop over `plan.actions` for `ActionParams::Synthesize { codec: Some(codec), .. }`.
2. For each synthesized track, if `codec_supported(target, codec) == Some(false)`, push `(synthetic_idx, codec)` to `offenders`. Use a synthetic index (e.g., `u32::MAX - n` or an enum-tagged identifier) so it doesn't collide with real track indices in the error message.
3. Skip when the synthesized action has `codec: None` (already-decoded upstream).
4. Error message copy: change "leave incompatible codecs in {filename}: {details}" to handle "(synthesized)" decoration for synthetic tracks.
5. Add unit tests covering both cases in the existing test module.

**Validation:** `cargo test -p voom-policy-evaluator` + `cargo clippy --workspace`.

---

### #42 — Use `init_and_register` for capability-collector

**Scope:** Small refactor — make ownership compatible.

**Files:**

- `crates/voom-kernel/src/lib.rs` — may need a new `init_and_register_with_handle()` or return the `Arc` from `init_and_register()`.
- `crates/voom-cli/src/app.rs` (lines 220–238) — switch to the unified path.

**Plan:**

1. Add a new kernel method `init_and_register_shared<P>(plugin: P, priority, ctx) -> Result<Arc<P>>` that: constructs the `Arc`, calls `init()`, registers with a clone, dispatches init events, returns the `Arc<P>` for caller use.
2. Migrate `capability-collector` in `app.rs:220-238` to use the new method and expose the `Arc<CapabilityCollectorPlugin>` via `BootstrapResult.collector`.
3. Consider migrating `report` plugin (app.rs:286-299) and `sqlite-store` (app.rs:136) to the same pattern for consistency.
4. Keep existing `init_and_register` for plugins that don't need a handle.

**Risks:** Minor API churn — confined to kernel+bootstrap.

**Validation:** `cargo test --workspace` + `cargo clippy --workspace`.

---

### #43 — StorageReporter sync-only contract

**Scope:** Doc comments only (for now).

**Files:**

- `plugins/job-manager/src/progress.rs` — the `StorageReporter` type.

**Plan:**

1. Add a module-level or type-level doc comment stating: "`StorageReporter::on_job_progress` performs a blocking SQLite write. Must only be called from synchronous/rayon contexts. If used from an async context, wrap in `tokio::task::spawn_blocking`."
2. Add `#[must_use]` where appropriate.
3. No runtime change. Re-evaluate when daemon-mode job processing moves to tokio tasks (tracked separately via #4).

**Validation:** `cargo doc --workspace --no-deps`.

---

### #47 — UNIX-compliant scripting output

**Scope:** Add `--format={table,json,tsv,csv,plain}` to list commands.

**Files:**

- `crates/voom-cli/src/commands/db.rs` (list-bad), `files.rs`, `plans.rs`, `jobs.rs`, `events.rs`, `report.rs` — any command producing a table.
- `crates/voom-cli/src/output.rs` (or new `crates/voom-cli/src/output/format.rs`) — shared formatter enum + writer.

**Plan:**

1. Add a shared `OutputFormat` enum (`Table`, `Json`, `Tsv`, `Csv`, `Plain`) with a `clap::ValueEnum` impl.
2. Add a `--format` flag (default `Table`) to each list command. Consider a top-level `--format` mirrored via `clap`'s `global = true`.
3. For each command, extract the current row assembly into a `Vec<Row>` where `Row` is command-specific, then render via the chosen format.
4. TSV output should be one record per line, tab-separated, no header by default (or add `--header`).
5. JSON output should serialize the command's existing domain types via `serde_json`.
6. Add integration tests for each new format (`assert_cmd` + snapshot).

**Validation:** `cargo test -p voom-cli` + functional tests.

**Note:** Consider adopting `--quiet`/`--porcelain` flag conventions from `git` for consistency.

---

### #93 — Avoid re-hashing unchanged files

**Scope:** Hash short-circuit based on (mtime, size).

**Files:**

- `plugins/discovery/src/scanner.rs` — hashing pipeline.
- `plugins/sqlite-store` — lookup by path to fetch cached (mtime, size, hash).
- `crates/voom-domain/src/storage.rs` — may need a `get_file_fingerprint(path) -> Option<(mtime, size, hash)>` helper.

**Plan:**

1. Add `StorageTrait::get_file_fingerprint(&self, path: &Path) -> Result<Option<FileFingerprint>>` where `FileFingerprint = { mtime_secs: i64, size: u64, content_hash: String }`.
2. In `discovery/scanner.rs`, before hashing, call the fingerprint lookup. If `mtime == stored_mtime && size == stored_size`, reuse the stored hash — skip the read.
3. Add a `--force-rescan` flag to invalidate the shortcut (the scan command already has one — plumb it through to discovery).
4. Emit a stat (`files_rehashed`, `files_skipped_by_fingerprint`) visible in the scan summary.
5. Tests: one for fingerprint match (no hash read), one for mtime change (re-hash), one for size change (re-hash), one for `--force-rescan` (always re-hash).

**Risks:** mtime resolution differs across filesystems — acceptable since changes >1s will always bust.

**Validation:** `cargo test -p voom-discovery -p voom-sqlite-store` + functional test timing.

---

### #28 — CSP drops `unpkg.com`; bundle htmx + Alpine

**Scope:** Vendor external assets, update CSP.

**Files:**

- `plugins/web-server/src/templates/*.html` — remove `<script src="https://unpkg.com/...">` tags.
- `plugins/web-server/static/vendor/` — add `htmx.min.js` and `alpine.min.js` (pinned versions).
- `plugins/web-server/src/router.rs` or `middleware.rs` — CSP header: remove `unpkg.com` from `script-src`, drop to `'self'` only.
- `plugins/web-server/build.rs` or a one-shot script — optional, to download+verify SHA256 on build.

**Plan:**

1. Download htmx 2.x and Alpine.js 3.x from their upstream GitHub releases, verify published SHA256, vendor as `plugins/web-server/static/vendor/htmx-<ver>.min.js` and `alpine-<ver>.min.js`.
2. Update all templates to reference `/static/vendor/htmx.min.js` and `/static/vendor/alpine.min.js`.
3. Update CSP in `middleware.rs` (search for `"unpkg.com"`): `script-src 'self'`.
4. Add a comment near the vendored files documenting the version and upstream URL.
5. Add a test: fetch the dashboard, assert the CSP header contains `script-src 'self'` and no `unpkg.com`.

**Validation:** `cargo test -p voom-web-server` + manual browser check (no CSP violations, dashboard functional).

---

### #30 — Minimum entropy check on web auth token

**Scope:** Startup warning only.

**Files:**

- `plugins/web-server/src/auth.rs` (or `config.rs`) — config validation step.

**Plan:**

1. Where `AuthConfig` is loaded, check `token.len() < 32` and emit `tracing::warn!` with the suggestion to use `openssl rand -base64 32`.
2. Optionally also check for low-entropy patterns (all same char, digits-only) — cheap Shannon entropy approximation, warn if < 3 bits/byte.
3. Test: construct config with short token, assert warning via `tracing-test`.

**Validation:** `cargo test -p voom-web-server`.

---

### #31 — Per-IP rate limiting on web API

**Scope:** Add `tower-governor` with a conservative default.

**Files:**

- `plugins/web-server/Cargo.toml` — add `tower_governor` dependency.
- `plugins/web-server/src/router.rs` (or `middleware.rs`) — add the layer, before `ConcurrencyLimitLayer`.
- `plugins/web-server/src/config.rs` — new `RateLimitConfig` (per-second, burst, enabled).

**Plan:**

1. Pick `tower-governor` v0.5+ (check current version at time of work).
2. Default config: 30 rps per IP, burst 60, enabled by default but documented as LAN-tuned.
3. Exempt `GET /static/*` (already served efficiently; excluding avoids page reload throttling).
4. Over-limit returns 429 with `Retry-After`.
5. Tests: 100 rapid requests from one IP → some get 429.

**Validation:** `cargo test -p voom-web-server` (integration test via `axum-test`).

---

## New plugin proposals (design summaries)

Each of these is a new plugin. Group together into "init-time events" sprint if capacity permits.

### #33 — `policy-validator` plugin

**Pattern:** Like `tool-detector`. At `init()`, load and validate the active `.voom` policy file, emit `PolicyLoaded` + zero-or-more `PolicyWarning` events. Plugins that currently re-parse the policy (phase-orchestrator, web-server dashboard) can subscribe instead.

**Files to add:** `plugins/policy-validator/{Cargo.toml, src/lib.rs, tests/}`. Event variants `PolicyLoaded`, `PolicyWarning` in `voom-domain/src/events.rs`.

**Downstream effort:** phase-orchestrator refactor to consume events rather than re-read the file.

### #35 — `library-index` plugin

**Pattern:** At `init()`, query `StorageTrait` for counts and emit `LibraryStatsReady`. Web dashboard subscribes for instant stats on first load.

**Files to add:** `plugins/library-index/{Cargo.toml, src/lib.rs}`. Event variant `LibraryStatsReady`.

**Dependency:** registration priority must come after `sqlite-store`.

### #36 — `fs-watcher` plugin

**Pattern:** Use `notify` crate. At `init()`, set up watchers on configured library roots, emit `WatchStarted` event. On FS changes, emit existing `FileDiscovered` — so the current pipeline needs no changes.

**Files to add:** `plugins/fs-watcher/{Cargo.toml, src/lib.rs}`. Event variant `WatchStarted`.

**Enables:** issue #4 (event-driven pipeline) without the polling design.

### #37 — `notifier` plugin

**Pattern:** Subscribes to configurable event types, forwards to Slack/webhook/desktop. At `init()`, validates channels and emits `NotificationChannelReady`.

**Files to add:** `plugins/notifier/{Cargo.toml, src/lib.rs}`. Event variant `NotificationChannelReady`. Config at `~/.config/voom/plugins/notifier/config.toml`.

**Covers:** part of #94 (*arr integration via webhooks).

### #131 — Backup directory cleanup

**Not strictly a new plugin** — this is a UX fix for `backup-manager`. Currently `find -name '*.voom-backup'` leaves clutter.

**Plan:**

1. On successful `finalize_backup`, if the backup dir is empty, remove it. Walk upward to remove empty parent backup dirs too, stopping at the library root.
2. Add a `voom backup clean [--dry-run]` subcommand that:
   - Scans for `.voom-backup` and `.vbak` files
   - Shows sizes and counts
   - Offers `--older-than <duration>` filtering
3. Expose the cleanup via the web UI (Settings → Backups panel).
4. Update the final-summary hint in `process.rs` to suggest the new subcommand, not a raw `find` pipe.

**Files:** `plugins/backup-manager/src/lib.rs`, new `crates/voom-cli/src/commands/backup.rs`, plus web-server dashboard card.

---

## Sprint-sized / design issues (defer, do not implement here)

### #4 — Event-driven pipeline under `serve`

**Status:** Large multi-phase feature. Should be scoped as its own sprint.

**Dependencies:** #33, #35, #36 (and arguably #37) should land first so the plugins exist. Then the `serve` command wires up a periodic scheduler and `POST /api/scan`.

**Plan sketch (high level):**

1. Add `ScanRequested` event variant.
2. Introduce `ServeConfig { scan_interval, scan_paths, auto_process }` in `config.toml`.
3. `serve` command spawns a tokio task that publishes `ScanRequested` on interval. Discovery plugin subscribes.
4. `POST /api/scan` publishes the same event.
5. SSE already broadcasts bus events → UI live updates work for free.
6. Migrate `StorageReporter` use-sites in daemon paths to `spawn_blocking` (per #43).

**Action:** Leave issue open. Create a scoped spec (`docs/sprints/sprint-13-event-pipeline.md`) before implementation.

### #90 — Container release options

**Status:** Ops/packaging track.

**Plan:** Provide a `Dockerfile` that:

1. Multi-stage build: `rust:1-bookworm` → build `voom` with `--release`, copy into `debian:stable-slim` with `ffmpeg`, `mkvtoolnix` installed.
2. `EXPOSE 8080`, `ENTRYPOINT ["voom", "serve"]`.
3. Volume mounts: `/data/config`, `/data/library`.
4. Publish to `ghcr.io/<org>/voom:<version>` via GitHub Actions.
5. Add a `docs/deployment/docker.md` with compose example.

**Action:** Implement post-Sprint-13 once the serve command is production-ready.

### #91 — Multi-host coordination

**Status:** Large feature, low priority until local execution is polished.

**Possible approaches to evaluate:** Raft-based coordinator, pull-based worker model with shared SQLite (low-hanging; same DB file over NFS), or gRPC agent protocol. Each has tradeoffs.

**Action:** Keep open. Revisit after #4 (event-driven serve) lands.

### #92 — Plugin stats

**Status:** Design-level — needs shape decided before implementation.

**Options:** (a) plugins own their data in `~/.config/voom/plugins/<name>/stats.db`; (b) extend `StorageTrait` with a generic `plugin_stats` table; (c) provide a stats-emission event type and let plugins self-report on an interval.

**Recommendation:** (c) — aligns with the init-time event pattern. Add `PluginStatsReported` event + a dashboard widget.

**Action:** Once #33/#35 land and the event pattern is solidified, convert this to a concrete plan.

### #94 — *arr stack integration

**Status:** Overlapping with #37 (notifier plugin).

**Action:** After #37 ships, close #94 as subsumed, or refocus it on a specific *arr-facing plugin (Radarr/Sonarr custom-script receivers, not just webhook out).

---

## Proposed execution order

Recommended work batches (each ~1 PR):

1. **Close-outs** — post verification comments and close #27, #34, #38, #39, #40, #41, #44. (No code.)
2. **Security trio** — #28, #30, #31. Small, independent, all touch `plugins/web-server`.
3. **Scripting output** — #47. Self-contained, CLI-only.
4. **Safeguard + refactor** — #42, #142. Both small, orthogonal.
5. **Docs-only** — #43. Trivial.
6. **Hash short-circuit** — #93. Discovery + storage. Moderate.
7. **Backup cleanup UX** — #131. Moderate.
8. **New plugins batch** — #33, #35, #36 together (same pattern).
9. **Notifier** — #37 (enables closing #94).
10. **Sprint 13** — #4 (event-driven serve), pulling in #43 async wrap.
11. **Ops track** — #90.
12. **Design tracks** — #91, #92 (await capacity).
