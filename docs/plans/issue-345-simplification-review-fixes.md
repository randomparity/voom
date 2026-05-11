# Issue 345 Simplification Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address all simplification-review findings on the issue #345 branch without changing the intended agent-friendly CLI behavior.

**Architecture:** Remove unused machine-output scaffolding, align the e2e consumer plan with the JSON shapes the CLI actually emits, make `policy validate --format json` consistent for `.voom` and `.toml` inputs, and update the remaining stale test call site from `--json` to `--format json`. Keep this as cleanup on top of the current branch, with one commit per recommendation.

**Tech Stack:** Rust 2024, clap, serde_json, assert_cmd integration tests, Bash/JQ plan examples, and the existing `cargo fmt`, `cargo test`, and `cargo clippy` verification flow.

---

## File Structure

- Modify `crates/voom-cli/src/output.rs`
  - Delete unused `MachineResponse<T>` and its tests.
  - Keep `print_json()` as the one shared JSON printing helper because commands now use it.
- Modify `docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md`
  - Change planned jobs fixtures and `jq` filters from a non-existent `data` wrapper to the actual `jobs` field.
  - Remove the claim that these examples use a `MachineResponse`-style wrapper.
- Modify `crates/voom-cli/src/commands/policy.rs`
  - Thread `OutputFormat` into `validate_policy_map()`.
  - Emit structured JSON for `.toml` policy-map validation.
- Modify `crates/voom-cli/tests/cli_tests.rs`
  - Add an integration test proving `policy validate map.toml --format json` emits parseable JSON.
- Modify `crates/voom-cli/tests/functional_tests.rs`
  - Replace the remaining stale `report --vmaf --json` test invocation with `report --vmaf --format json`.

## Task 1: Remove Unused `MachineResponse`

**Files:**
- Modify: `crates/voom-cli/src/output.rs`

- [ ] **Step 1: Delete the unused type and impl**

In `crates/voom-cli/src/output.rs`, remove this whole block:

```rust
// Introduced before command adoption so later phases can share one JSON contract.
#[allow(dead_code)]
#[derive(Debug, Serialize)]
pub struct MachineResponse<T>
where
    T: Serialize,
{
    pub command: &'static str,
    pub status: &'static str,
    pub data: T,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

// Introduced before command adoption so later phases can share one JSON contract.
#[allow(dead_code)]
impl<T> MachineResponse<T>
where
    T: Serialize,
{
    pub fn ok(command: &'static str, data: T) -> Self {
        Self {
            command,
            status: "ok",
            data,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }
}
```

- [ ] **Step 2: Keep `print_json()` and remove the dead-code allow**

Replace:

```rust
// Introduced before command adoption so later phases can replace ad hoc JSON printing.
#[allow(dead_code)]
pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
```

with:

```rust
pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
```

- [ ] **Step 3: Delete the unused tests**

In the `#[cfg(test)] mod tests` block in `crates/voom-cli/src/output.rs`, remove:

```rust
use serde_json::json;

#[test]
fn machine_response_serializes_success_envelope() {
    let response = MachineResponse::ok("jobs.list", json!({"jobs": []}));
    let value = serde_json::to_value(response).unwrap();

    assert_eq!(value["command"], "jobs.list");
    assert_eq!(value["status"], "ok");
    assert_eq!(value["data"], json!({"jobs": []}));
    assert_eq!(value["warnings"], json!([]));
    assert_eq!(value["errors"], json!([]));
}

#[test]
fn machine_response_serializes_empty_list_data() {
    let response = MachineResponse::ok("plugin.list", Vec::<String>::new());
    let value = serde_json::to_value(response).unwrap();

    assert_eq!(value["data"], json!([]));
}

#[test]
fn machine_response_serializes_warnings() {
    let response =
        MachineResponse::ok("env.check", json!({"passed": false})).with_warning("vmaf missing");
    let value = serde_json::to_value(response).unwrap();

    assert_eq!(value["warnings"], json!(["vmaf missing"]));
}
```

- [ ] **Step 4: Verify the type is gone**

Run:

```bash
rg -n "MachineResponse|with_warning|success_envelope" crates/voom-cli/src/output.rs
```

Expected: no matches and exit code 1.

- [ ] **Step 5: Run focused Rust checks**

Run:

```bash
cargo test -p voom-cli output::
cargo clippy -p voom-cli --all-targets --all-features -- -D warnings
```

Expected: output tests pass, and clippy exits 0 with no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/voom-cli/src/output.rs
git commit -m "refactor(cli): remove unused machine response"
```

## Task 2: Align The E2E Consumer Plan With Actual Jobs JSON

**Files:**
- Modify: `docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md`

- [ ] **Step 1: Replace planned jobs fixtures with the actual top-level `jobs` field**

Replace the completed-jobs fixture:

```json
{
  "data": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "completed"}
  ]
}
```

with:

```json
{
  "jobs": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "completed"}
  ],
  "counts": [["completed", 2]],
  "limit": 50,
  "offset": 0
}
```

Replace the straggler fixture:

```json
{
  "data": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "running"}
  ]
}
```

with:

```json
{
  "jobs": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "running"}
  ],
  "counts": [["completed", 1], ["running", 1]],
  "limit": 50,
  "offset": 0
}
```

- [ ] **Step 2: Replace `jq` filters that read `.data`**

Replace:

```bash
jq -e '.data[]? | select(.status == "running" or .status == "pending")' \
```

with:

```bash
jq -e '.jobs[]? | select(.status == "running" or .status == "pending")' \
```

Replace:

```bash
completed_jobs=$(jq '[.data[]? | select(.status == "completed")] | length' "${jobs_json}")
failed_jobs=$(jq '[.data[]? | select(.status == "failed")] | length' "${jobs_json}")
```

with:

```bash
completed_jobs=$(jq '[.jobs[]? | select(.status == "completed")] | length' "${jobs_json}")
failed_jobs=$(jq '[.jobs[]? | select(.status == "failed")] | length' "${jobs_json}")
```

- [ ] **Step 3: Remove the stale `MachineResponse` assertion from self-review**

Replace:

```markdown
- Type consistency: All JSON examples use the existing `MachineResponse`-style `data` wrapper expected from the new CLI output helpers.
```

with:

```markdown
- Type consistency: Jobs JSON examples use the actual `jobs`, `counts`, `limit`, and `offset` fields emitted by `voom jobs list --format json`.
```

- [ ] **Step 4: Verify the plan no longer references the wrong shape**

Run:

```bash
rg -n "MachineResponse|\\.data\\[\\]|\"data\"" docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md
```

Expected: no matches and exit code 1.

- [ ] **Step 5: Commit**

```bash
git add docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md
git commit -m "docs(scripts): align e2e jobs json plan"
```

## Task 3: Make Policy Map Validation JSON-Aware

**Files:**
- Modify: `crates/voom-cli/src/commands/policy.rs`
- Modify: `crates/voom-cli/tests/cli_tests.rs`

- [ ] **Step 1: Add a failing integration test**

In `crates/voom-cli/tests/cli_tests.rs`, add this test near the other policy validation tests:

```rust
#[test]
fn test_policy_validate_map_json_is_parseable() {
    let dir = tempfile::tempdir().unwrap();
    let policy = dir.path().join("minimal.voom");
    let policy_map = dir.path().join("map.toml");

    std::fs::write(
        &policy,
        r#"policy "minimal" {
  phase containerize {
    container mkv
  }
}
"#,
    )
    .unwrap();

    std::fs::write(
        &policy_map,
        r#"default = "minimal.voom"

[[mapping]]
prefix = "movies"
policy = "minimal.voom"
"#,
    )
    .unwrap();

    let json = assert_stdout_is_json(&[
        "policy",
        "validate",
        policy_map.to_str().unwrap(),
        "--format",
        "json",
    ]);

    assert_eq!(json["valid"], true);
    assert_eq!(json["policy_count"], 1);
    assert!(json["policies"].is_array());
    assert_eq!(json["policies"][0]["policy"], "minimal");
}
```

- [ ] **Step 2: Run the failing test**

Run:

```bash
cargo test -p voom-cli --test cli_tests test_policy_validate_map_json_is_parseable
```

Expected: FAIL because `policy validate map.toml --format json` currently prints human text for policy maps.

- [ ] **Step 3: Thread `format` into policy-map validation**

In `crates/voom-cli/src/commands/policy.rs`, replace:

```rust
if file.extension().is_some_and(|e| e == "toml") {
    return validate_policy_map(file);
}
```

with:

```rust
if file.extension().is_some_and(|e| e == "toml") {
    return validate_policy_map(file, format);
}
```

Replace the function signature:

```rust
fn validate_policy_map(file: &std::path::Path) -> Result<()> {
```

with:

```rust
fn validate_policy_map(file: &std::path::Path, format: OutputFormat) -> Result<()> {
```

- [ ] **Step 4: Emit JSON for policy maps**

Inside `validate_policy_map()`, after:

```rust
let policies = resolver.policies();
```

insert:

```rust
if matches!(format, OutputFormat::Json) {
    let policies: Vec<serde_json::Value> = policies
        .iter()
        .map(|(name, compiled)| {
            serde_json::json!({
                "name": name,
                "policy": compiled.name,
                "phase_count": compiled.phases.len(),
                "phase_order": compiled.phase_order,
            })
        })
        .collect();
    output::print_json(&serde_json::json!({
        "valid": true,
        "policy_map": file,
        "policy_count": policies.len(),
        "policies": policies,
    }))?;
    return Ok(());
}
```

Keep the existing human table output below this new JSON branch.

- [ ] **Step 5: Run the focused test again**

Run:

```bash
cargo test -p voom-cli --test cli_tests test_policy_validate_map_json_is_parseable
```

Expected: PASS.

- [ ] **Step 6: Run related CLI tests**

Run:

```bash
cargo test -p voom-cli --test cli_tests test_new_query_json_outputs_are_parseable test_policy_validate_map_json_is_parseable
```

Expected: both tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/voom-cli/src/commands/policy.rs crates/voom-cli/tests/cli_tests.rs
git commit -m "fix(cli): emit json for policy maps"
```

## Task 4: Update Remaining Stale `--json` Test Call Site

**Files:**
- Modify: `crates/voom-cli/tests/functional_tests.rs`

- [ ] **Step 1: Replace the stale report alias call**

In `crates/voom-cli/tests/functional_tests.rs`, replace:

```rust
.args(["report", "--vmaf", "--json"])
.output()
.expect("run report --vmaf --json");
```

with:

```rust
.args(["report", "--vmaf", "--format", "json"])
.output()
.expect("run report --vmaf --format json");
```

- [ ] **Step 2: Verify no stale report alias remains**

Run:

```bash
rg -n 'report.*--json|--vmaf.*--json|run report --vmaf --json' crates/voom-cli/tests
```

Expected: no matches and exit code 1.

- [ ] **Step 3: Run the focused test**

Run:

```bash
cargo test -p voom-cli --test functional_tests vmaf_summary_json_is_stable
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/voom-cli/tests/functional_tests.rs
git commit -m "test(cli): update vmaf report json format"
```

## Task 5: Final Verification

**Files:**
- Verify all modified files.

- [ ] **Step 1: Format check**

Run:

```bash
cargo fmt --all --check
```

Expected: no output and exit code 0.

- [ ] **Step 2: Run the CLI package tests**

Run:

```bash
cargo test -p voom-cli
```

Expected: all `voom-cli` tests pass.

- [ ] **Step 3: Run clippy**

Run:

```bash
cargo clippy -p voom-cli --all-targets --all-features -- -D warnings
```

Expected: no warnings and exit code 0.

- [ ] **Step 4: Run targeted stale-shape searches**

Run:

```bash
rg -n "MachineResponse|\\.data\\[\\]|report.*--json|--vmaf.*--json" \
  crates/voom-cli/src \
  crates/voom-cli/tests \
  docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md
```

Expected: no matches except parser-rejection tests that intentionally mention `--json`; if those appear, confirm they are rejection tests and not command invocations.

- [ ] **Step 5: Check repository state**

Run:

```bash
git status --short
```

Expected: clean working tree.

## Self-Review

- Spec coverage: The plan maps one task to each simplification-review recommendation: unused `MachineResponse`, e2e consumer shape mismatch, policy-map JSON consistency, and stale `--json` test usage.
- Placeholder scan: No task contains deferred implementation language or unspecified tests.
- Type consistency: The e2e plan uses the actual `jobs` JSON object shape from `crates/voom-cli/src/commands/jobs.rs`; the policy-map JSON test asserts the same fields emitted by the planned implementation.
