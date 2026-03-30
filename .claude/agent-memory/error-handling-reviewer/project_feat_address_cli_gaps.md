---
name: feat/address-cli-gaps-1 branch error handling review
description: Key error handling patterns and findings from the feat/address-cli-gaps-1 branch (new CLI commands: config, db stats, events, health, plans show, files show/delete, --plan-only)
type: project
---

## New production-code panicking expressions introduced in this branch

### MEDIUM-risk (production code, non-test)
- `crates/voom-cli/src/app.rs:107` — `.expect("store is Some after successful init")` on the store handle after successful init. Justified: the invariant is established two lines above. LOW risk in practice, but technically a panic.
- `crates/voom-cli/src/commands/config.rs:28` — `.expect("line contains '=' (checked above)")` in `show()`. Justified: `.contains('=')` check is the if-condition on line 25. LOW risk.
- `crates/voom-cli/src/commands/health.rs:379` — `.expect("midnight is always valid")` on `and_hms_opt(0, 0, 0)`. Justified: hardcoded values, always valid. LOW risk.
- `crates/voom-cli/src/commands/plans.rs:56` — `.expect("PlanSummary serialization cannot fail")` on `serde_json::to_string_pretty`. MEDIUM risk: `PlanSummary` contains user-controlled strings that could trigger edge cases. Should use `?` instead.
- `crates/voom-cli/src/commands/files.rs:63` — `.expect("MediaFile serialization cannot fail")` on `serde_json::to_string_pretty`. Same concern as above.
- `crates/voom-process/src/lib.rs:26-27` — `.expect("stdout piped")` / `.expect("stderr piped")`. Justified: `Stdio::piped()` is set two lines above in the same function; the take() can only fail if already taken. LOW risk.

### Process.rs String error convention (pre-existing, documented)
- `process.rs:418` and `process.rs:546` — `Result<Option<serde_json::Value>, String>` return type. Pre-existing documented pattern; the worker pool's processor signature forces this. See project_architectural_decisions.md.

## New commands: error handling quality
- `events.rs` — uses `unwrap_or_default()` on config load (line 10), silently falls back to default config on error. Same pattern as `health.rs:61` and `health.rs:322`. This degrades silently; the user doesn't know their config file is broken.
- `events.rs` — `serde_json::from_str(...).unwrap_or_else(...)` on event payload (line 102). Safe: falls back to storing raw string. OK.
- `health.rs` — calls `std::process::Command::new("ffmpeg")` directly (lines 186-187, 266-267) without using `voom_process` timeout wrapper. In `print_hw_accel_status()` only; low impact (diagnostic command, not data pipeline).
- `plans.rs` and `files.rs` — serialize with `.expect()` on serde. Should use `?` with `anyhow::Error`.
- `db.rs:289` — `std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)` silently returns 0 if db doesn't exist yet. Acceptable for a stats display.

## Config show() `.expect()` (MEDIUM)
`config.rs:28` — the `show()` function parses config lines looking for `auth_token =` and uses `line.find('=').expect(...)` to find the index. The guard `trimmed.contains('=')` uses `trimmed` (the whitespace-stripped line) but the `find('=')` call is on `line` (the original). These are the same underlying string data so they will always agree, but the code is subtly fragile. It would be clearer to use `line.split_once('=')`.

**How to apply:** When reviewing config.rs or suggesting changes, note the `.find('=').expect(...)` pattern could be simplified with `split_once`.
