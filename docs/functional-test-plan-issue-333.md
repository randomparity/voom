# Functional Test Plan: Failed Plans Surface In Job Accounting

Issue: <https://github.com/randomparity/voom/issues/333>

## Goal

Verify that an executable plan failure is reflected as a failed file job and an
unambiguous process summary, even when `--on-error continue` lets the batch keep
running.

## Corpus Setup

Generate a small corpus with multiple H.264 inputs so one induced executor
failure can be compared with successful files in the same run:

```sh
scripts/generate-test-corpus /tmp/voom-issue-333-corpus \
  --profile coverage \
  --only basic-h264-aac,letterbox-h264 \
  --duration 2 \
  --seed 333
```

Use `docs/examples/continue-on-error-transcode.voom` as the policy.

## Execution

Run once with a deliberately constrained or broken executor environment for one
file, then run the same command after restoring the executor. In local manual
testing, the failure can be induced by temporarily moving one generated file
away after discovery in a debugger, by injecting a failing executor in a test
build, or by using an ffmpeg wrapper that exits non-zero for one selected input.

```sh
voom process /tmp/voom-issue-333-corpus \
  --policy docs/examples/continue-on-error-transcode.voom \
  --on-error continue \
  --workers 2
```

## Assertions

1. The final process summary reports a non-zero error count when any executable
   plan fails.
2. The phase breakdown reports the failed phase with the same non-zero error
   count.
3. `voom jobs list --status failed` includes the file job whose plan failed.
4. `voom report errors --session <session>` includes the plan failure details.
5. The command may still exit `0` with `--on-error continue`; the visible
   summary and job/reporting state are the source of truth for partial failure.

## Adversarial Review

- A skipped plan must not fail the file job; only failed executable or
  safeguard plans should affect job failure accounting.
- `--on-error continue` must continue processing other files after one file job
  fails.
- `--on-error fail` should still cancel the batch through the worker pool once
  a failed plan causes the file job to return an error.
