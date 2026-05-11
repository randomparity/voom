# Issue 345 Agent-Friendly CLI Plan

Issue: <https://github.com/randomparity/voom/issues/345>
Suggested branch: `feat/issue-345-agent-friendly-cli`

## Problem

The CLI needs a tree-wide pass to make commands predictable for agents and
scripts. The current CLI already has several machine-readable paths through
`OutputFormat::{Json, Plain, Csv}`, but support is uneven: some commands have
only human output, some use command-specific `--json` booleans, progress
suppression is only partially tied to machine output, and success/error messages
are not represented through a shared machine-readable contract.

Because VOOM is pre-release, this work should replace inconsistent interfaces
instead of adding compatibility shims.

## Goals

- Every read/query command can emit valid JSON without unrelated human text on
  stdout.
- Every mutating command has deterministic non-interactive behavior through
  existing global `--yes` or command-level confirmation flags.
- Machine-readable output has stable object shapes with command, status, data,
  warnings, and errors where applicable.
- Progress, diagnostics, and deprecation notices go to stderr, not stdout.
- Help text makes automation affordances discoverable without marketing copy.
- Tests lock in stdout/stderr separation, JSON validity, parse behavior, and
  representative empty-result shapes.

## Non-Goals

- Do not add a new CLI framework or dependency.
- Do not build a separate agent protocol or daemon API.
- Do not preserve old pre-release flag aliases when a cleaner interface
  replaces them.
- Do not turn every human table into an identical schema in one giant refactor;
  normalize shared command plumbing first, then high-value command groups.

## Current CLI Surfaces

- Parser and command tree: `crates/voom-cli/src/cli.rs`
- Dispatch, quiet behavior, lock policy: `crates/voom-cli/src/main.rs`
- Formatting helpers: `crates/voom-cli/src/output.rs`
- Command implementations: `crates/voom-cli/src/commands/*.rs`
- Integration-style CLI assertions: `crates/voom-cli/tests/cli_tests.rs`
- User-facing docs: `docs/cli-reference.md`

## Proposed Interface Rules

1. Keep `--format <table|json|plain|csv>` as the single output selector for
   commands that return data.
2. Remove command-specific `--json` booleans where `--format json` can express
   the same behavior.
3. Add `--format json` to read/query commands that currently lack it:
   `policy list`, `policy validate`, `policy show`, `policy diff`,
   `plugin list`, `plugin info`, `jobs list`, `jobs status`, `config show`,
   `config get`, and `bug-report` status-like paths if needed.
4. Keep commands that primarily edit files or launch services human-only unless
   there is a concrete data result to return.
5. When a command runs in a machine format, stdout must contain only the chosen
   data format. Human progress, warnings, and deprecation notices must use
   stderr.
6. Empty JSON results should use the natural empty shape for the command:
   arrays for list commands and objects with `status` plus empty fields for
   status/summary commands.
7. JSON errors should be added only if there is an explicit machine-output
   request. Clap parse errors can remain clap-native unless a global error
   format is introduced in a later issue.

## Output Envelope

Use a small shared envelope for commands that currently only print human status
or summaries:

```json
{
  "command": "jobs.list",
  "status": "ok",
  "data": {},
  "warnings": [],
  "errors": []
}
```

Do not wrap domain data that already has a clear stable JSON shape unless the
command also needs status/warnings/errors. For example, `inspect --format json`
can continue returning a serialized `MediaFile`, while `policy validate
--format json` should return a validation result object.

## Implementation Tasks

### Task 1: CLI Inventory and Contract Tests

Files:
- Modify: `crates/voom-cli/tests/cli_tests.rs`
- Create: `docs/cli-agent-friendliness-audit.md`

Steps:
1. Add a focused CLI smoke matrix in `cli_tests.rs` that runs representative
   commands with `--help`, default output, and JSON output where already
   supported.
2. Add helpers:
   - `assert_stdout_is_json(command)`
   - `assert_no_human_status_on_json_stdout(command)`
   - `assert_human_notes_on_stderr(command)`
3. Document the current command inventory in
   `docs/cli-agent-friendliness-audit.md` with columns:
   command, mutates state, supports `--format`, supports JSON, empty JSON shape,
   prompts, progress output, notes.
4. Run:
   ```sh
   cargo test -p voom-cli --test cli_tests
   ```
5. Commit:
   ```sh
   git add crates/voom-cli/tests/cli_tests.rs docs/cli-agent-friendliness-audit.md
   git commit -m "test(cli): inventory agent-facing command output"
   ```

### Task 2: Shared Machine Output Helpers

Files:
- Modify: `crates/voom-cli/src/output.rs`
- Modify: `crates/voom-cli/src/cli.rs`

Steps:
1. Add tests in `output.rs` for serializing a success envelope, an empty list,
   and a warning-bearing response.
2. Add a small `CommandStatus` or `MachineResponse<T>` type in `output.rs` with
   `command`, `status`, `data`, `warnings`, and `errors`.
3. Add `print_json<T: serde::Serialize>(&T) -> anyhow::Result<()>` to centralize
   pretty JSON output and avoid repeated `expect` calls at display boundaries.
4. Keep `OutputFormat::is_machine()` and use it for quiet/progress decisions.
5. Run:
   ```sh
   cargo test -p voom-cli output::tests
   cargo test -p voom-cli cli::tests::test_output_format_is_machine
   ```
6. Commit:
   ```sh
   git add crates/voom-cli/src/output.rs crates/voom-cli/src/cli.rs
   git commit -m "feat(cli): add shared machine output helpers"
   ```

### Task 3: Normalize Format Flags

Files:
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/commands/report.rs`
- Modify: `crates/voom-cli/src/commands/env.rs`
- Modify: `crates/voom-cli/src/commands/policy.rs`

Steps:
1. Replace command-specific `--json` booleans on `report`, `env check`, and
   `policy test` with `--format json` unless a command has a strong reason for
   a dedicated boolean.
2. Add parser tests that old `--json` flags are rejected after replacement.
3. Add `--format` to high-value read commands that lack it:
   `policy list`, `policy validate`, `policy show`, `policy diff`,
   `plugin list`, `plugin info`, `jobs list`, `jobs status`, `config show`,
   and `config get`.
4. Leave mutating commands without JSON unless they have a useful result object.
5. Run:
   ```sh
   cargo test -p voom-cli cli::tests
   ```
6. Commit:
   ```sh
   git add crates/voom-cli/src/cli.rs \
     crates/voom-cli/src/commands/report.rs \
     crates/voom-cli/src/commands/env.rs \
     crates/voom-cli/src/commands/policy.rs
   git commit -m "feat(cli): normalize machine output flags"
   ```

### Task 4: Stdout/Stderr Separation

Files:
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/scan.rs`
- Modify: `crates/voom-cli/src/commands/process/mod.rs`
- Modify: `crates/voom-cli/src/commands/inspect.rs`
- Modify: `crates/voom-cli/src/commands/events.rs`
- Modify: `crates/voom-cli/src/commands/report.rs`
- Modify: `crates/voom-cli/src/commands/verify.rs`
- Modify: `crates/voom-cli/src/commands/backup.rs`
- Modify: `crates/voom-cli/src/commands/tools.rs`
- Modify: `crates/voom-cli/src/commands/env.rs`

Steps:
1. Replace ad hoc quiet detection in `main.rs` with an `effective_quiet`
   function that checks `cli.quiet` and machine formats across the full command
   tree.
2. Add unit tests for `effective_quiet` covering `scan --format json`,
   `inspect --format json`, `events --format json`, `report --format json`,
   and a normal table command.
3. Move human-only informational messages from stdout to stderr where they can
   accompany data output.
4. Ensure empty JSON outputs are still printed to stdout, not suppressed by
   quiet mode.
5. Run:
   ```sh
   cargo test -p voom-cli main::tests
   cargo test -p voom-cli --test cli_tests
   ```
6. Commit:
   ```sh
   git add crates/voom-cli/src/main.rs \
     crates/voom-cli/src/commands/scan.rs \
     crates/voom-cli/src/commands/process/mod.rs \
     crates/voom-cli/src/commands/inspect.rs \
     crates/voom-cli/src/commands/events.rs \
     crates/voom-cli/src/commands/report.rs \
     crates/voom-cli/src/commands/verify.rs \
     crates/voom-cli/src/commands/backup.rs \
     crates/voom-cli/src/commands/tools.rs \
     crates/voom-cli/src/commands/env.rs
   git commit -m "fix(cli): keep machine stdout parseable"
   ```

### Task 5: JSON Coverage for Query Commands

Files:
- Modify: `crates/voom-cli/src/commands/policy.rs`
- Modify: `crates/voom-cli/src/commands/plugin.rs`
- Modify: `crates/voom-cli/src/commands/jobs.rs`
- Modify: `crates/voom-cli/src/commands/config.rs`

Steps:
1. For each command, add tests that execute JSON mode and parse stdout with
   `serde_json`.
2. Use natural JSON data:
   - `policy validate`: `{ "valid": true, "errors": [] }`
   - `policy list`: array of policy descriptors
   - `policy diff`: structured diff summary, with fixture plan diff when used
   - `plugin list`: array of registered/disabled plugin descriptors
   - `plugin info`: plugin descriptor object
   - `jobs list`: array plus pagination metadata
   - `jobs status`: single job descriptor or not-found error
   - `config show`: redacted config object
   - `config get`: `{ "key": "...", "value": ... }`
3. Do not parse table output in tests. Assert behavior, not table layout.
4. Run:
   ```sh
   cargo test -p voom-cli --test cli_tests
   cargo test -p voom-cli commands::policy
   cargo test -p voom-cli commands::plugin
   cargo test -p voom-cli commands::jobs
   cargo test -p voom-cli commands::config
   ```
5. Commit:
   ```sh
   git add crates/voom-cli/src/commands/policy.rs \
     crates/voom-cli/src/commands/plugin.rs \
     crates/voom-cli/src/commands/jobs.rs \
     crates/voom-cli/src/commands/config.rs \
     crates/voom-cli/tests/cli_tests.rs
   git commit -m "feat(cli): add JSON output for query commands"
   ```

### Task 6: Non-Interactive Safety Review

Files:
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/backup.rs`
- Modify: `crates/voom-cli/src/commands/db.rs`
- Modify: `crates/voom-cli/src/commands/files.rs`
- Modify: `crates/voom-cli/src/commands/jobs.rs`

Steps:
1. Verify every destructive command accepts either global `--yes` or a
   command-level `--yes`, and that global `--yes` is honored consistently.
2. Add tests for declining prompts and accepting through global `--yes`.
3. Make refusal messages clear and actionable on stderr.
4. Do not add new force flags unless a command has no existing confirmation
   path.
5. Run:
   ```sh
   cargo test -p voom-cli main::tests::test_global_yes
   cargo test -p voom-cli --test cli_tests
   ```
6. Commit:
   ```sh
   git add crates/voom-cli/src/main.rs \
     crates/voom-cli/src/commands/backup.rs \
     crates/voom-cli/src/commands/db.rs \
     crates/voom-cli/src/commands/files.rs \
     crates/voom-cli/src/commands/jobs.rs
   git commit -m "fix(cli): make confirmations automation friendly"
   ```

### Task 7: Help and Documentation

Files:
- Modify: `docs/cli-reference.md`
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `docs/INDEX.md` if the new audit document should be listed

Steps:
1. Update help strings so output flags describe machine-friendly use directly:
   `--format json` for structured output, `--format plain` for line-oriented
   paths, and `--quiet` for suppressing progress/status.
2. Document stdout/stderr rules in `docs/cli-reference.md`.
3. Document JSON support command by command.
4. Link `docs/cli-agent-friendliness-audit.md` from `docs/INDEX.md` only if the
   audit is intended to remain as maintained documentation.
5. Run:
   ```sh
   cargo test -p voom-cli cli::tests
   ```
6. Commit:
   ```sh
   git add crates/voom-cli/src/cli.rs docs/cli-reference.md docs/INDEX.md
   git commit -m "docs(cli): document agent-friendly output"
   ```

### Task 8: Final Verification

Run:

```sh
cargo fmt --all --check
cargo test -p voom-cli
cargo clippy -p voom-cli --all-targets --all-features -- -D warnings
```

Manual smoke checks:

```sh
cargo run -p voom-cli -- --help
cargo run -p voom-cli -- env check --format json | jq .
cargo run -p voom-cli -- tools list --format json | jq .
cargo run -p voom-cli -- policy validate docs/examples/minimal.voom --format json | jq .
cargo run -p voom-cli -- plugin list --format json | jq .
```

Before opening the PR, re-read the diff for duplicate output code. If the same
JSON/table branching appears in three command modules, move only the common
printing primitive into `output.rs`; keep command-specific data construction in
the command modules.

## Open Questions

- Should `inspect --format json` remain a raw `MediaFile`, or should it move
  into an envelope for consistency? Recommendation: keep it raw because it is
  already direct domain data and likely useful to scripts.
- Should clap parse errors support JSON? Recommendation: defer. It requires
  global error handling and is less valuable than ensuring successful
  machine-output commands are parseable.
- Should `plain` and `csv` be available everywhere JSON is? Recommendation:
  only for list-like commands where line-oriented output has a natural shape.

## Acceptance Criteria

- `cargo test -p voom-cli --test cli_tests` includes coverage for every command
  group that claims JSON support.
- All commands documented with `--format json` produce valid JSON on stdout with
  no human prose mixed in.
- Human progress/status/deprecation messages are on stderr.
- Destructive commands have a tested non-interactive path.
- `docs/cli-reference.md` matches the implemented command tree.
