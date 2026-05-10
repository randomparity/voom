# Issue 205 Adversarial Review

## Plan Review

- Scope control: PASS. The implementation keeps estimate records separate from
  executable plan status and does not overload `plans.status`.
- Generated corpus coverage: PASS. The functional plan and smoke verification
  use `scripts/generate-test-corpus` to create local testable media.
- User-facing docs: PASS. `docs/preflight-estimates.md`, `docs/cli-reference.md`,
  and example policies describe `--estimate`, `--estimate-only`,
  `voom estimate`, `voom estimate calibrate`, and `--confirm-savings`.
- Example policy accuracy: PASS. The preflight examples use existing DSL
  transcode/container syntax and are covered by policy test suites.

## Code Review

- Executor dispatch boundary: PASS. `process --estimate` uses the dry-run path
  and does not dispatch `PlanCreated`. `--confirm-savings` skips below-threshold
  plans before executor dispatch.
- What-if persistence boundary: PASS. Estimate runs are stored in
  `estimate_runs` and `estimate_files`, not in `plans`, so they cannot appear as
  pending executable work.
- Codec/backend attribution: PASS. Estimate output now includes a transcode
  breakdown keyed by codec and backend.
- Parallelism: PASS. Wall time divides compute time by effective workers with a
  minimum of one worker.
- Net-byte-loss visibility: PASS. Negative estimated savings increments the
  net-loss count and is reported separately from aggregate bytes saved.
- Uncertainty reporting: PASS with caveat. High-uncertainty estimates print a
  rough aggregate range. The current range is a conservative display heuristic,
  not a statistically derived p25/p75 interval.
- Calibration: PASS with caveat. `voom estimate calibrate` records persistent
  local samples. It seeds conservative defaults rather than running a hardware
  benchmark suite.
- Accuracy thresholds: PARTIAL. The code can use persisted samples, but local
  verification did not run an actual transcode accuracy comparison asserting
  ±20% bytes and ±30% time on a calibrated corpus.

## Documentation Review

- Command behavior: PASS. Docs state estimate mode does not execute plans.
- Savings gate safety: PASS. Docs state below-threshold plans are skipped before
  executor dispatch.
- Corpus instructions: PASS. The functional plan gives the exact
  `scripts/generate-test-corpus` command used to produce test media.
- Promise boundary: PASS. Docs describe high uncertainty as directional and do
  not claim estimate-vs-actual reporting beyond persisted what-if records.

## Verification Evidence

- `cargo test -p voom-domain estimate::tests -- --nocapture`
- `cargo test -p voom-sqlite-store estimate_storage -- --nocapture`
- `cargo test -p voom-cli estimate -- --nocapture`
- `cargo clippy -p voom-cli --all-targets --all-features -- -D warnings`
- `cargo test -p voom-policy-testing -- --nocapture`
- `cargo test -p voom-cli policy -- --nocapture`
- `scripts/generate-test-corpus /tmp/voom-estimate-corpus-smoke --profile smoke --count 2 --seed 205 --duration 1`
- `XDG_CONFIG_HOME=/tmp/voom-estimate-config cargo run -p voom-cli -- --force estimate /tmp/voom-estimate-corpus-smoke --policy docs/examples/preflight-archive.voom --workers 2`

## Follow-up Risk

The web UI confirmation dialog from the issue body is not implemented in this
branch. If it remains out of this PR, file a follow-up issue before merge and
run the same plan, implementation, review, and PR workflow for that issue.
