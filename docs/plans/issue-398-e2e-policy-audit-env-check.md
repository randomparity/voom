# Issue 398 E2E Policy Audit Env Check Plan

Issue: <https://github.com/randomparity/voom/issues/398>

## Problem

The e2e policy audit harness still invokes the deprecated top-level command:

```sh
voom doctor
```

That command now prints this compatibility warning before the real environment
checks:

```text
warning: `voom doctor` is deprecated; use `voom env check` instead
```

Because `scripts/e2e-policy-audit/run.sh` captures command output into
`logs/doctor.log`, every new run directory now includes a stale deprecation
warning in the preflight logs.

## Scope

Update only the e2e policy audit harness and its directly related docs/tests.
Do not remove or change the deprecated `voom doctor` alias in the CLI; it is
still covered elsewhere as a compatibility surface.

## Recommended Approach

Rename the harness artifact from `doctor` to `env-check` while changing the
command to `voom env check`.

This makes the generated run directory match the command that produced it and
avoids carrying the deprecated terminology forward in new logs. Since existing
historical run directories may still contain `logs/doctor.log.rc`, the summary
builder should accept `doctor` as a fallback only when `env-check` is absent.

## Files

- Modify `scripts/e2e-policy-audit/run.sh`
  - Replace `log_run doctor "${voom_bin}" doctor` with
    `log_run env-check "${voom_bin}" env check`.
- Modify `scripts/e2e-policy-audit/lib/build-summary.sh`
  - Change the hard-check loop to require `env-check`, `policy-validate`, and
    `scan` for new runs.
  - Keep a compatibility fallback that reads `doctor.log.rc` only if
    `env-check.log.rc` is missing.
  - Report failures using the modern name (`env-check`) so summaries do not
    reintroduce deprecated terminology.
- Modify `scripts/e2e-policy-audit/tests/test.sh`
  - Update summary test fixtures to write `logs/env-check.log.rc`.
  - Add a focused compatibility fixture proving an older run with only
    `logs/doctor.log.rc` still summarizes successfully.
- Modify `scripts/e2e-policy-audit/README.md`
  - Mention `voom env check` as one of the captured CLI preflight checks if a
    command list is added or already present.
- Optional cleanup: update stale design/archive docs only if the branch policy
  expects generated design docs to track current command names. The active code
  and harness README should be the source of truth.

## Implementation Tasks

### Task 1: Update Harness Invocation

1. In `scripts/e2e-policy-audit/run.sh`, replace the `doctor` log invocation
   with `env-check`.
2. Verify the generated files for a new run will be:
   - `logs/env-check.log`
   - `logs/env-check.log.rc`
3. Confirm no new `logs/doctor.log` is produced by `run.sh`.

### Task 2: Update Summary Hard Checks

1. In `scripts/e2e-policy-audit/lib/build-summary.sh`, introduce a helper for
   resolving log rc files:
   - prefer `${run}/logs/env-check.log.rc`
   - fall back to `${run}/logs/doctor.log.rc`
   - fail if neither exists
2. Keep `policy-validate` and `scan` behavior unchanged.
3. Ensure non-zero status from either `env-check.log.rc` or the fallback
   `doctor.log.rc` marks the summary as failed.

### Task 3: Update Tests

1. Change existing summary tests in
   `scripts/e2e-policy-audit/tests/test.sh` from:

   ```sh
   for log_name in doctor policy-validate scan; do
   ```

   to:

   ```sh
   for log_name in env-check policy-validate scan; do
   ```

2. Add one small compatibility test that creates only
   `logs/doctor.log.rc`, runs `lib/build-summary.sh`, and asserts the hard
   criteria pass.
3. Run:

   ```sh
   scripts/e2e-policy-audit/tests/test.sh
   ```

### Task 4: Documentation Sweep

1. Run:

   ```sh
   rg -n "voom doctor|doctor\\.log|log_name in doctor|log_run doctor" scripts/e2e-policy-audit docs/plans/issue-345-e2e-agent-friendly-cli-consumer.md
   ```

2. Update active harness docs and plans that describe current e2e harness
   behavior.
3. Leave CLI compatibility docs/tests alone unless they are specifically
   describing the e2e policy audit harness.

## Verification

Run the focused harness tests:

```sh
scripts/e2e-policy-audit/tests/test.sh
```

Run a command-level smoke check without a full media-library e2e run:

```sh
tmp_run=$(mktemp -d)
mkdir -p "${tmp_run}/logs" "${tmp_run}/reports" "${tmp_run}/db-export" "${tmp_run}/diffs"
printf '0\n' >"${tmp_run}/logs/env-check.log.rc"
printf '0\n' >"${tmp_run}/logs/policy-validate.log.rc"
printf '0\n' >"${tmp_run}/logs/scan.log.rc"
cat >"${tmp_run}/diffs/files-summary.md" <<'EOF'
# Snapshot Diff Summary

Disappeared paths: 0
Missing backup post-run: 0
EOF
scripts/e2e-policy-audit/lib/build-summary.sh "${tmp_run}" 0 0
rg -n "doctor|deprecated" "${tmp_run}/summary.md" "${tmp_run}/logs" || true
```

Expected results:

- New harness runs use `voom env check`.
- New run directories contain `logs/env-check.log` rather than
  `logs/doctor.log`.
- `summary.md` no longer references the deprecated `doctor` command for new
  runs.
- Historical run directories with only `doctor.log.rc` can still be summarized.
