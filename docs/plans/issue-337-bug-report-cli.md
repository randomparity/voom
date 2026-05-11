# Issue 337 Bug Report CLI Plan

Issue: <https://github.com/randomparity/voom/issues/337>
Branch: `feat/issue-337-bug-report-cli`

## Problem

Users need a safe way to capture VOOM diagnostics when filing bug reports. The
report must include relevant system information, plan/policy context, and run
results without leaking private filenames, library paths, media-library
contents, tokens, keys, or credentials. Redactions must be stable across the
entire generated output, and generation must be separate from upload so users
can review the sanitized files first.

## Proposed CLI

```sh
voom bug-report generate \
  --out /tmp/voom-bug \
  --session <uuid> \
  --policy policy.voom \
  --library ~/Movies

voom bug-report upload /tmp/voom-bug \
  --issue 337 \
  --repo randomparity/voom
```

`generate` should work with no optional filters and capture general
environment, config, database summaries, recent jobs, recent events, and
current errors. `upload` should consume only sanitized files from a previously
generated report directory.

## Architecture

Add a new top-level `voom bug-report` command group in `voom-cli`.

Generation is local-first. It writes a transparent directory containing:

- `report.md`: sanitized human-readable issue-comment content.
- `report.json`: sanitized machine-readable diagnostics.
- `redactions.public.json`: placeholder inventory without originals.
- `redactions.local.json`: private original-to-placeholder map, never uploaded.
- `README.txt`: review/upload instructions.

Upload uses the existing `gh` CLI:

```sh
gh issue comment <issue> --repo <owner/name> --body-file <generated-body>
```

Do not add a GitHub API dependency in the first implementation. Keeping upload
behind `gh` leaves authentication and GitHub-specific credential storage outside
VOOM.

## Files

- Modify `crates/voom-cli/src/cli.rs` for `BugReportCommands`,
  `BugReportGenerateArgs`, and `BugReportUploadArgs`.
- Modify `crates/voom-cli/src/main.rs` to dispatch the command. Leave
  `command_needs_lock` unchanged because these commands are read-only from
  VOOM's database and library perspective.
- Modify `crates/voom-cli/src/commands/mod.rs` to expose `bug_report`.
- Create `crates/voom-cli/src/commands/bug_report/mod.rs` for orchestration.
- Create `crates/voom-cli/src/commands/bug_report/redactor.rs` for stable
  redaction mapping and recursive JSON/string redaction.
- Create `crates/voom-cli/src/commands/bug_report/collect.rs` for diagnostics
  collection.
- Create `crates/voom-cli/src/commands/bug_report/render.rs` for local report
  file output.
- Create `crates/voom-cli/src/commands/bug_report/upload.rs` for sanitized
  upload through `gh`.
- Modify `docs/cli-reference.md`.
- Create `docs/functional-test-plan-issue-337.md`.

## Redaction Rules

Use a single stateful redactor for the entire bundle so repeated values map to
the same replacement.

- Video-like filenames become `video000.mkv`, `video001.mp4`, preserving the
  extension.
- Non-video path components become `path000`, `path001`.
- Secret values become typed placeholders such as `<api-key-001>`,
  `<token-001>`, or `<secret-001>`.
- Secret key detection should include `token`, `api_key`, `apikey`, `secret`,
  `password`, `credential`, and `bearer`.
- When a secret assignment is detected, register both the key/value occurrence
  and the raw value so later occurrences without the key are redacted to the
  same placeholder.
- `redactions.local.json` may contain originals for user review. `upload.rs`
  must never read it.

## Collection Scope

Collect:

- VOOM version from `env!("VOOM_PRODUCT_VERSION")`.
- OS, architecture, current time, and redacted current working directory.
- A limited environment snapshot: `VOOM_*`, `RUST_LOG`, and known tool-path
  variables only. Do not dump arbitrary environment variables.
- Sanitized `AppConfig` through `serde_json::to_value(config::load_config()?)`.
- Optional policy file contents through `std::fs::read_to_string`, redacted
  before writing.
- Optional library root as a redacted path only. Do not enumerate media files
  directly from disk.
- Database row counts through `table_row_counts`.
- Recent jobs through `list_jobs` with a configurable limit.
- Recent events through `list_event_log` with a configurable limit.
- Latest health checks through `latest_health_checks`.
- If `--session` is provided, keep only captured events whose raw or parsed
  payload contains that session UUID.

If opening the database fails, include a sanitized `"storage": "unavailable"`
section with the error message instead of failing the whole report.

## Implementation Tasks

### Task 1: CLI Surface

1. Add parser tests in `crates/voom-cli/src/cli.rs` for:
   - `voom bug-report generate --out /tmp/voom-bug --session <uuid> --policy policy.voom --library /media/movies`
   - `voom bug-report upload /tmp/voom-bug --issue 337 --repo randomparity/voom`
2. Run `cargo test -p voom-cli cli::tests::test_bug_report_` and confirm RED.
3. Add `Commands::BugReport(BugReportCommands)`.
4. Add `BugReportGenerateArgs` with `out`, `session`, `policy`, `library`,
   `event_limit`, and `job_limit`.
5. Add `BugReportUploadArgs` with `report_dir`, `issue`, and `repo`.
6. Add `commands::bug_report::run` dispatch in `main.rs`.
7. Run the parser tests and confirm GREEN.

### Task 2: Stable Redactor

1. Add tests that prove:
   - `"The Movie (2026).mkv"` maps to the same `video000.mkv` replacement
     across multiple strings.
   - `api_key=sk-123456` and later `sk-123456` both map to
     `<api-key-001>`.
   - recursive JSON redaction handles nested paths and secret keys.
2. Run `cargo test -p voom-cli commands::bug_report::redactor::tests` and
   confirm RED.
3. Implement `Redactor`, `RedactionReport`, `PrivateRedactionMapping`,
   `PublicRedactionMapping`, and `RedactionKind`.
4. Run the redactor tests and confirm GREEN.

### Task 3: Diagnostics Collection

1. Add collection tests for policy-file redaction and restricted environment
   capture.
2. Run `cargo test -p voom-cli commands::bug_report::collect::tests` and
   confirm RED.
3. Implement `BugReportBundle`, environment/config capture, optional policy
   capture, storage summaries, recent jobs/events, health checks, and session
   filtering.
4. Ensure all collected strings/JSON values pass through the same `Redactor`.
5. Run collection tests and confirm GREEN.

### Task 4: Local Rendering

1. Add render tests that verify sanitized files do not contain original private
   values while `redactions.local.json` does.
2. Run `cargo test -p voom-cli commands::bug_report::render::tests` and confirm
   RED.
3. Implement output directory validation and write `report.md`, `report.json`,
   `redactions.public.json`, `redactions.local.json`, and `README.txt`.
4. Refuse to write into unrelated non-empty directories. Allow overwriting only
   known report files when a previous `metadata.json` identifies
   `"kind": "voom_bug_report"`.
5. Run render tests and confirm GREEN.

### Task 5: Upload

1. Add upload tests proving `build_issue_body` reads `report.md` and excludes
   `redactions.local.json`.
2. Run `cargo test -p voom-cli commands::bug_report::upload::tests` and confirm
   RED.
3. Implement upload by calling `gh issue comment`.
4. Provide an actionable error if `gh` is missing or unauthenticated.
5. Run upload tests and confirm GREEN.

### Task 6: Docs

1. Document `voom bug-report generate` and `voom bug-report upload` in
   `docs/cli-reference.md`.
2. Create `docs/functional-test-plan-issue-337.md` covering:
   - a corpus with a real-looking filename,
   - a policy containing that filename and a fake token,
   - report generation,
   - absence of originals in `report.md` and `report.json`,
   - presence of originals only in `redactions.local.json`,
   - upload to a test issue through `gh`.
3. Run `cargo test -p voom-cli cli::tests::verify_cli`.

### Task 7: Verification

Run:

```sh
cargo test -p voom-cli commands::bug_report
cargo test -p voom-cli cli::tests::test_bug_report_
cargo test -p voom-cli cli::tests::verify_cli
cargo fmt --check
cargo clippy -p voom-cli --all-targets --all-features -- -D warnings
```

Smoke-test generation:

```sh
cargo run -p voom-cli -- bug-report generate --out /tmp/voom-issue-337-smoke
```

Confirm the smoke report contains the expected files and tells the user to
review the local output before upload.

Check the upload security boundary:

```sh
rg "redactions\\.local|PrivateRedaction|original" \
  crates/voom-cli/src/commands/bug_report/upload.rs
```

Expected: no matches except a test assertion proving private values are
excluded from the upload body.

## Non-Goals

- No native GitHub API client in the first implementation.
- No `.zip` or `.tar` report archive in the first implementation.
- No direct media-library enumeration from `--library`.
- No automatic upload during generation.
