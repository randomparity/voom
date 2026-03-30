---
name: VOOM Security Audit Context
description: Audit findings, verified-safe patterns, and known gaps across multiple branch reviews
type: project
---

Security audits conducted across multiple branches through 2026-03-29.

**Why:** Ongoing security review cadence. Track what has been verified safe vs. what needs ongoing attention.

**How to apply:** Re-audit these surfaces when the noted areas change.

## Audit 1: desloppify/code-health branch (2026-03-28)

Branch refactored ToolOutput/HttpResponse into voom-domain::host_types, refactored voom-process, updated web API handlers, and changed ffprobe to use voom-process.

Re-audit triggers: host function path checks, chunked_delete callers, web auth middleware scope, WASM resource limit configuration.

## Audit 2: fix/multi-gh-issues branch (2026-03-28)

Branch added: per-IP rate limiting (governor crate), auth token weak-token warning, bundled static assets (htmx + Alpine.js), health check API + storage, executor capability probing via ffmpeg/mkvmerge subprocess invocations at init, discovered_files SQLite table.

Re-audit triggers: rate limiter DashMap growth policy, static asset update procedure, health check details field content, executor init blocking behavior.

## Audit 3: feat/address-cli-gaps-1 branch (2026-03-29)

Branch added: voom config get/set, voom db stats, voom events (with --follow), voom files list/show/delete, voom health check/history, voom jobs retry/clear/--offset, voom plans show, voom backup list/restore/cleanup, voom tools, bus-tracer plugin.

Re-audit triggers: event log filter path (user string → SQL LIKE), backup restore path derivation, health.rs direct ffmpeg invocation, table_row_counts SQL string interpolation.

## Verified Safe (cumulative)

- **Command injection**: All executors (ffmpeg, mkvtoolnix, ffprobe) use `std::process::Command::new().args()` with discrete arg pushes. No shell invocation, no string concatenation.
- **Metadata sanitization**: `validate_metadata_value` / `validate_metadata_key` in `voom-domain::utils::sanitize` applied before all metadata args.
- **SQLite parameterization**: All rusqlite queries use `params![]` or `?N` positional parameters. LIKE wildcards escaped via `escape_like()`. The `list_health_checks` SQL builder uses `format!()` only for `?N` placeholder indices (not for values).
- **chunked_delete table/column args**: Now uses `PruneTarget` enum with `&'static str` returns — table/column names are compile-time constants via enum dispatch. No user-supplied values reach SQL structure.
- **table_row_counts SQL interpolation** (maintenance_storage.rs:101): Uses `format!("SELECT COUNT(*) FROM {table}")` where `table` is iterated from a hardcoded `&'static str` array. Not injectable — all values are compile-time literals.
- **list_event_log SQL builder**: `event_type` filter from user is used as a LIKE parameter value only. SQL structure (`LIKE ?N ESCAPE '\\'`) is built from format strings with only the placeholder index (`?N`) interpolated. Wildcard escaping via `escape_like()` applied. Safe.
- **Web auth**: `is_authorized` uses `subtle::ConstantTimeEq`. Auth middleware covers all routes including SSE, HTML pages, and static asset routes via `route_layer`. Static assets merged into `authenticated_routes` — confirmed in tests.
- **WASM sandboxing**: Memory limit 256 MiB, epoch interruption, 1 MiB stack, StoreLimitsBuilder. `allowed_tools` empty-deny-all enforced.
- **CSP**: Nonce-based per-request, no `unsafe-inline`, no `unsafe-eval`, no external CDN references. External scripts were unpkg.com previously — now bundled locally.
- **Static asset supply chain**: htmx 2.0.4 and Alpine.js 3.14.8 bundled via `include_bytes!` at compile time. sha256 hashes: htmx = e209dda5c8235479f3166defc7750e1dbcd5a5c1808b7792fc2e6733768fb447, alpine = b600e363d99d95444db54acbfb2deffec9ae792aa99a09229bcda078e5b55643.
- **Rate limiting**: `RateLimitLayer` is the outermost Tower layer (applied before auth and concurrency limit). General: 120 req/min/IP. CPU-intensive (/api/policy/validate, /api/policy/format): 10 req/min/IP. CPU-intensive paths also consume general quota.
- **Token weak-token warning**: Logs token length (not value) when < 32 chars. Warning goes to tracing log, not HTTP response.
- **Health check endpoint**: `/api/health` is inside `authenticated_routes` (requires auth when configured). Returns `latest_health_checks()` only — no user-supplied filter parameters.
- **Executor probed codec/format matching**: Uses iterator `.any()` with `==` comparison — no injection surface.
- **CPU-intensive path matching**: `uri().path() == exact_string` — query strings excluded by axum URI parsing, so bypass via `?foo=bar` is not possible.
- **MkvtoolnixExecutorPlugin availability gate**: `available` field defaults to `false`; only set to `true` after `mkvmerge --version` succeeds in `init()`. Prevents stale-executor handling.
- **backup_path_for filename sanitization**: `file_name()` used to extract filename, then `replace(['/', '\\', '\0'], "_")` applied before building backup path. Prevents path traversal in backup target.
- **backup restore (derive_original_name)**: `file_name()` on backup path before extracting original name ensures no directory separators. Original name is a filename-only string joined to a known parent directory. No traversal possible via `/` or `\`.
- **bus-tracer output path**: Loaded from plugin config (TOML file), not from event bus or user input. Output path is `expand_tilde()` expanded and written to by the plugin; no user-controlled write target.
- **voom config set validation**: All values deserialized into `AppConfig` before write (line 140). Type coercion rejects arrays and tables; booleans and integers are parse-validated.
- **voom events event_type filter → SQL**: User-supplied `--filter` value is used only as a LIKE parameter value. SQL structure uses `format!()` only for the `?N` index. LIKE wildcards escaped via `escape_like()`. Parameterized correctly.

## Known Gaps / Open Issues

1. **WASM `read-file-metadata` path bypass** (Medium): Path check uses `starts_with()` on non-canonicalized path. A plugin with `allowed_paths = ["/media"]` can access `/media/../etc/passwd`. `run_tool` host function uses `canonicalize()` correctly; `read-file-metadata` and `list-files` do not.

2. **`chunked_delete` SQL injection** (Resolved): Now safe via `PruneTarget` enum with static strings. Not injectable.

3. **Governor DashMap unbounded growth** (Low/informational): `RateLimitLayer` uses `DashMapStateStore<IpAddr>` with no periodic cleanup. In a public-facing deployment with many unique IPs, this grows without bound. Acceptable for stated LAN-only deployment model.

4. **Executor init blocks async runtime** (Low/operational): `bootstrap_kernel_with_store` is a sync fn called from `async serve::run`. The ffmpeg executor runs 3 blocking subprocesses (`-codecs`, `-formats`, `-hwaccels`) during `init()`. This is startup-only, not per-request, but will stall the tokio worker for the init duration.

5. **Token entropy not enforced** (partially addressed): Weak-token warning now logged at startup when token < 32 chars. Enforcement (rejection) is not done — still caller-supplied.

6. **Static asset hashes not pinned in documentation** (Low/informational): The bundled JS files are baked in at compile time (stronger than CDN), but SHA-256 hashes are not recorded in any CLAUDE.md or docs file to facilitate future version verification.

7. **`voom health check` invokes `ffmpeg` by name** (Low/informational): `health.rs` uses `std::process::Command::new("ffmpeg")` directly (lines 186, 266, 269) rather than the tool-detector's resolved path. If an attacker can inject a malicious `ffmpeg` into `$PATH`, this would execute it. This is a local privilege escalation concern, not a remote one. The CLI is a local tool run by the user; `$PATH` manipulation is already within the attacker's capabilities if they have local access.

8. **`voom config edit` uses `$EDITOR` env var** (Informational): `config.rs:76` uses `std::env::var("EDITOR")` as the editor binary name. This is standard Unix behavior; `$EDITOR` injection requires local access and is within the user's own security boundary. Not a remote concern. The resulting config is validated by `load_config()` after editing.
