# Issue 333: Failed Plan Accounting

Issue: <https://github.com/randomparity/voom/issues/333>
Branch: `fix/issue-333-plan-failure-accounting`

## Plan

1. Track failed executable plans inside the per-file process pipeline.
2. Continue evaluating the current file according to existing dependency and
   `run_if` semantics, but return a file-job error after the phase loop if any
   executable plan failed.
3. Let the existing worker-pool error strategy decide whether the batch
   continues (`--on-error continue`) or cancels (`--on-error fail`).
4. Keep the process command exit-code behavior unchanged for
   `--on-error continue`; make the summary and job/reporting state
   unambiguous.
5. Add regression tests, example policy coverage, generated-corpus functional
   test instructions, and adversarial review notes.

## Implementation

The process pipeline now counts failed executable/safeguard plans and returns
an error for the file job after the phase loop. This routes the failure through
the existing worker-pool path, marking the job failed and incrementing the
summary error count without changing plan failure event recording.

## Functional Test

See `docs/functional-test-plan-issue-333.md`. It uses
`scripts/generate-test-corpus` and
`docs/examples/continue-on-error-transcode.voom` to validate user-visible
summary, jobs, and report behavior.

## Acceptance Criteria Review

Add a regression test where one file has a failed plan and `--on-error continue`
is active:

- Covered by a focused pipeline test that forces an executor failure and by the
  generated-corpus functional plan for batch behavior.

CLI summary uses plan/file failure counts consistently:

- A failed executable plan now makes the file job fail, so the worker-pool error
  count and phase breakdown both become non-zero.

Jobs/reporting distinguish successful jobs from jobs with failed plans, or
expose a `partial` outcome:

- Implemented by marking the file job failed when any executable plan fails.
  A dedicated `partial` status is deferred until a UI/API design requires it.

Decide and document exit-code semantics for `--on-error continue`:

- Exit `0` remains intentional for continued batch runs. The process summary,
  `voom jobs list --status failed`, and `voom report errors --session <id>`
  expose the partial failure.

## Deferred Work

No follow-up issue is required for `partial` job status yet. The current
accepted behavior uses existing failed job status, and no user-facing API has
been designed around a separate partial state.

## Adversarial Review

- Risk: returning early on first plan failure would skip useful downstream
  accounting. Mitigation: the pipeline records all phase outcomes first, then
  returns the job error at the end.
- Risk: skipped or empty phases could be misclassified as failures. Mitigation:
  the counter is incremented only on explicit plan/safeguard failure paths.
- Risk: changing exit codes could break existing automation. Mitigation:
  `--on-error continue` keeps command completion semantics unchanged while
  making failure state visible in summaries and jobs.
