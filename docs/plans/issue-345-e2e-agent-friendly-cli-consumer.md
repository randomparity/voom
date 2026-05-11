# E2E Agent-Friendly CLI Consumer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update the e2e policy audit scripts to consume the new agent-friendly `--format json` command shape for low-risk internal harness outputs.

**Architecture:** Keep the audit harness behavior unchanged for operators, but make its machine-consumed artifacts JSON-first. `run.sh` remains the producer of run artifacts; `build-summary.sh` becomes the first internal consumer of `voom jobs list --format json`; `preflight.sh` captures `voom config show --format json` as a redacted environment artifact.

**Tech Stack:** Bash with `set -euo pipefail`, `jq`, existing VOOM CLI commands, and the current `scripts/e2e-policy-audit/tests/test.sh` fixture test harness.

---

## File Structure

- Modify `scripts/e2e-policy-audit/run.sh`
  - Change low-risk VOOM invocations to the new command shape:
    - `policy validate <file> --format json`
    - `jobs list --format json`
    - `report --all --format json`
  - Save JSON artifacts as `.json` files and keep raw command logs under `logs/`.
- Modify `scripts/e2e-policy-audit/lib/build-summary.sh`
  - Read `reports/jobs.json` with `jq` for straggler and failed-job checks.
  - Keep `reports/jobs.txt` as a fallback only for older run directories.
- Modify `scripts/e2e-policy-audit/lib/preflight.sh`
  - Capture `voom config show --format json` into `env/voom-config.redacted.json`.
  - Keep the existing TOML redaction snapshot as a compatibility/readability artifact.
- Modify `scripts/e2e-policy-audit/tests/test.sh`
  - Change the summary fixture from `reports/jobs.txt` to `reports/jobs.json`.
  - Add a straggler fixture so the JSON status check is covered by behavior, not just syntax.
- Modify `scripts/e2e-policy-audit/README.md`
  - Document the JSON artifacts produced by the harness.

## Task 1: Teach Summary Tests About `jobs.json`

**Files:**
- Modify: `scripts/e2e-policy-audit/tests/test.sh`
- No production code changes in this task.

- [ ] **Step 1: Replace the completed-jobs text fixture with JSON**

In `run_summary_failed_phase_test`, replace:

```bash
  cat >"${actual}/reports/jobs.txt" <<'EOF'
job-1 completed
job-2 completed
EOF
```

with:

```bash
  cat >"${actual}/reports/jobs.json" <<'EOF'
{
  "jobs": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "completed"}
  ],
  "counts": [["completed", 2]],
  "limit": 50,
  "offset": 0
}
EOF
```

- [ ] **Step 2: Add a failing behavior test for JSON stragglers**

Add this function after `run_summary_failed_phase_test`:

```bash
run_summary_jobs_json_straggler_test() {
  local actual
  local summary

  actual=$(mktemp -d)
  trap 'rm -R "${actual}"' EXIT

  mkdir -p \
    "${actual}/logs" \
    "${actual}/reports" \
    "${actual}/db-export" \
    "${actual}/diffs"

  for log_name in doctor policy-validate scan; do
    printf '0\n' >"${actual}/logs/${log_name}.log.rc"
  done

  cat >"${actual}/diffs/files-summary.md" <<'EOF'
# Snapshot Diff Summary

Disappeared paths: 0
Missing backup post-run: 0
EOF

  cat >"${actual}/reports/jobs.json" <<'EOF'
{
  "jobs": [
    {"id": "job-1", "status": "completed"},
    {"id": "job-2", "status": "running"}
  ],
  "counts": [["completed", 1], ["running", 1]],
  "limit": 50,
  "offset": 0
}
EOF

  "lib/build-summary.sh" "${actual}" 2 2

  summary="${actual}/summary.md"
  if ! grep -Fq "FAIL: jobs report contains non-terminal states (running/pending)" "${summary}"; then
    echo "FAIL: jobs.json straggler was not reported" >&2
    fail=1
  fi

  rm -R "${actual}"
  trap - EXIT
}
```

Then call it immediately after `run_summary_failed_phase_test`:

```bash
run_summary_failed_phase_test
run_summary_jobs_json_straggler_test
```

- [ ] **Step 3: Run the focused script test and verify it fails**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: failure with `FAIL: jobs.json straggler was not reported`, because `build-summary.sh` does not read `reports/jobs.json` yet.

- [ ] **Step 4: Commit the failing test**

```bash
git add scripts/e2e-policy-audit/tests/test.sh
git commit -m "test(scripts): cover json jobs summary"
```

## Task 2: Consume `jobs.json` In The Summary Builder

**Files:**
- Modify: `scripts/e2e-policy-audit/lib/build-summary.sh`
- Test: `scripts/e2e-policy-audit/tests/test.sh`

- [ ] **Step 1: Add explicit JSON and text job report paths**

In `build-summary.sh`, replace:

```bash
# Job stragglers
jobs_report="${run}/reports/jobs.txt"
if [[ -f "${jobs_report}" ]]; then
  if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
    note_fail "jobs report contains non-terminal states (running/pending)"
  fi
fi
```

with:

```bash
# Job stragglers
jobs_json="${run}/reports/jobs.json"
jobs_report="${run}/reports/jobs.txt"
if [[ -f "${jobs_json}" ]]; then
  if jq -e '.jobs[]? | select(.status == "running" or .status == "pending")' \
    "${jobs_json}" >/dev/null; then
    note_fail "jobs report contains non-terminal states (running/pending)"
  fi
elif [[ -f "${jobs_report}" ]]; then
  if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
    note_fail "jobs report contains non-terminal states (running/pending)"
  fi
fi
```

- [ ] **Step 2: Convert failed-plan job status checks to prefer JSON**

In the `if ((total_failed_plans > 0)); then` block, replace:

```bash
  if [[ -f "${jobs_report}" ]] &&
    grep -qE 'completed:[[:space:]]+[1-9][0-9]*' "${jobs_report}" &&
    ! grep -qE 'failed:[[:space:]]+[1-9][0-9]*' "${jobs_report}"; then
    note_warn "jobs report has completed jobs but no failed jobs despite ${total_failed_plans} failed plan(s)"
  fi
```

with:

```bash
  if [[ -f "${jobs_json}" ]]; then
    completed_jobs=$(jq '[.jobs[]? | select(.status == "completed")] | length' "${jobs_json}")
    failed_jobs=$(jq '[.jobs[]? | select(.status == "failed")] | length' "${jobs_json}")
    if ((completed_jobs > 0 && failed_jobs == 0)); then
      note_warn "jobs report has completed jobs but no failed jobs despite ${total_failed_plans} failed plan(s)"
    fi
  elif [[ -f "${jobs_report}" ]] &&
    grep -qE 'completed:[[:space:]]+[1-9][0-9]*' "${jobs_report}" &&
    ! grep -qE 'failed:[[:space:]]+[1-9][0-9]*' "${jobs_report}"; then
    note_warn "jobs report has completed jobs but no failed jobs despite ${total_failed_plans} failed plan(s)"
  fi
```

- [ ] **Step 3: Run the focused script test**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: PASS with no `diff -u` output and no `FAIL:` lines.

- [ ] **Step 4: Run shell lint for touched scripts**

Run:

```bash
shellcheck scripts/e2e-policy-audit/lib/build-summary.sh scripts/e2e-policy-audit/tests/test.sh
```

Expected: no output and exit code 0.

- [ ] **Step 5: Commit the summary consumer**

```bash
git add scripts/e2e-policy-audit/lib/build-summary.sh scripts/e2e-policy-audit/tests/test.sh
git commit -m "fix(scripts): consume json jobs report"
```

## Task 3: Produce JSON Artifacts From The E2E Harness

**Files:**
- Modify: `scripts/e2e-policy-audit/run.sh`
- Test: `scripts/e2e-policy-audit/tests/test.sh`

- [ ] **Step 1: Update policy validation to use the new JSON command shape**

In `run.sh`, replace:

```bash
log_run policy-validate "${voom_bin}" policy validate "${policy}"
```

with:

```bash
log_run policy-validate "${voom_bin}" policy validate "${policy}" --format json
```

- [ ] **Step 2: Save the jobs query as JSON**

In `run.sh`, replace:

```bash
log_run jobs-list "${voom_bin}" jobs list
cp "${run_dir}/logs/jobs-list.log" "${run_dir}/reports/jobs.txt"
```

with:

```bash
log_run jobs-list "${voom_bin}" jobs list --format json
cp "${run_dir}/logs/jobs-list.log" "${run_dir}/reports/jobs.json"
```

- [ ] **Step 3: Save the report query as JSON**

In `run.sh`, replace:

```bash
log_run report "${voom_bin}" report --all
cp "${run_dir}/logs/report.log" "${run_dir}/reports/report.txt"
```

with:

```bash
log_run report "${voom_bin}" report --all --format json
cp "${run_dir}/logs/report.log" "${run_dir}/reports/report.json"
```

- [ ] **Step 4: Run the focused script test**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: PASS with no `diff -u` output and no `FAIL:` lines.

- [ ] **Step 5: Run shell lint for the producer script**

Run:

```bash
shellcheck scripts/e2e-policy-audit/run.sh
```

Expected: no output and exit code 0.

- [ ] **Step 6: Commit the producer changes**

```bash
git add scripts/e2e-policy-audit/run.sh
git commit -m "chore(scripts): write e2e cli json artifacts"
```

## Task 4: Capture JSON Config In Preflight

**Files:**
- Modify: `scripts/e2e-policy-audit/lib/preflight.sh`

- [ ] **Step 1: Add a JSON config snapshot next to the existing TOML snapshot**

In `preflight.sh`, after the existing config redaction block:

```bash
config_file="${config_dir}/config.toml"
if [[ -r "${config_file}" ]]; then
    sed -E 's/(password|token|secret|key|credential)([[:space:]]*=[[:space:]]*).*/\1\2"<redacted>"/I' \
        "${config_file}" >"${env_dir}/voom-config.redacted.toml" || true
fi
```

add:

```bash
"${voom_bin}" config show --format json >"${env_dir}/voom-config.redacted.json" 2>/dev/null || true
```

- [ ] **Step 2: Run shell lint for preflight**

Run:

```bash
shellcheck scripts/e2e-policy-audit/lib/preflight.sh
```

Expected: no output and exit code 0.

- [ ] **Step 3: Commit the preflight snapshot**

```bash
git add scripts/e2e-policy-audit/lib/preflight.sh
git commit -m "chore(scripts): capture json config snapshot"
```

## Task 5: Document The New JSON Artifacts

**Files:**
- Modify: `scripts/e2e-policy-audit/README.md`

- [ ] **Step 1: Update the artifact tree**

Find the artifact tree section that includes:

```markdown
├── env/                          tool versions, GPU state, policy copy, redacted config
├── reports/                      voom report --all, files, plans, jobs, events.json
```

Replace it with:

```markdown
├── env/                          tool versions, GPU state, policy copy, redacted config JSON/TOML
├── reports/                      VOOM JSON reports, files CSV, plans JSON, jobs JSON, events JSON
```

- [ ] **Step 2: Add a short compatibility note**

After the artifact tree, add:

```markdown
The harness stores machine-consumed VOOM CLI outputs as JSON. Raw command
output remains available in `logs/`, where each command also has a matching
`.log.rc` file for exit-code checks.
```

- [ ] **Step 3: Run a docs diff review**

Run:

```bash
git diff -- scripts/e2e-policy-audit/README.md
```

Expected: the README only describes the new artifact names and JSON behavior; it should not describe unimplemented options or change harness usage.

- [ ] **Step 4: Commit the docs update**

```bash
git add scripts/e2e-policy-audit/README.md
git commit -m "docs(scripts): document e2e json artifacts"
```

## Task 6: Final Verification

**Files:**
- Verify all modified files.

- [ ] **Step 1: Run script behavior tests**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: PASS with no `diff -u` output and no `FAIL:` lines.

- [ ] **Step 2: Run shell lint**

Run:

```bash
shellcheck \
  scripts/e2e-policy-audit/run.sh \
  scripts/e2e-policy-audit/lib/build-summary.sh \
  scripts/e2e-policy-audit/lib/preflight.sh \
  scripts/e2e-policy-audit/tests/test.sh
```

Expected: no output and exit code 0.

- [ ] **Step 3: Run CLI package tests as regression coverage**

Run:

```bash
cargo test -p voom-cli
```

Expected: all `voom-cli` tests pass.

- [ ] **Step 4: Check repository state**

Run:

```bash
git status --short
```

Expected: clean working tree.

## Self-Review

- Spec coverage: The plan updates the low-priority script consumers identified in this branch: jobs JSON consumption, policy validation JSON output, report JSON output, and config JSON snapshot. It leaves raw command logs intact.
- Placeholder scan: No task uses `TBD`, vague error handling, or unspecified tests.
- Type consistency: Jobs JSON examples use the actual `jobs`, `counts`, `limit`, and `offset` fields emitted by `voom jobs list --format json`.
