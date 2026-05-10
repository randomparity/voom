# Issue #205: Pre-flight Cost Estimation for Policy Runs

## Goal

Add a pre-flight estimator that evaluates policy plans without executing them,
prints phase/backend/codec cost estimates, persists the estimate as a what-if
record, and lets users block runs that do not meet a savings threshold.

## Scope

Implement the CLI-first feature requested by issue #205:

- `voom process <path> --estimate` and `--estimate-only` print an estimate and
  do not execute plans.
- `voom estimate <path> --policy ...` prints the same estimate through a
  standalone command.
- `voom process ... --confirm-savings <SIZE>` executes only files whose
  estimated per-file savings meet the threshold.
- Estimates include phase, codec, backend, bytes-in, bytes-out, bytes saved,
  compute time, wall time adjusted for workers, uncertainty, high-uncertainty
  warnings, and net-byte-loss flags.
- Estimates are persisted as what-if records so later code can compare
  estimate-vs-actual.
- `voom estimate calibrate` records local calibration samples in the same cost
  model table used by estimator lookups.

Web UI estimate dialogs are also part of the issue text, but they should land
after the CLI/storage model is reviewable. Do not merge the branch until either
the web UI slice is implemented or a follow-up issue exists for the deferred
work.

## Architecture

Add estimate domain types in `voom-domain` and keep the cost model deterministic
and explainable. The first model reads historical `TranscodeOutcome` rows when
available and falls back to conservative defaults keyed by phase, codec, preset,
and backend. The CLI produces normal `Plan` values, passes them to the estimator,
persists one estimate run plus per-file details, then either exits (`--estimate`)
or filters execution through `--confirm-savings`.

The storage layer gets dedicated estimate tables instead of overloading plan
status. What-if records should survive without dispatching `PlanCreated`, because
estimate-only runs must not be interpreted as pending work.

## Implementation Plan

### 1. Domain model and estimator

Files:

- Create `crates/voom-domain/src/estimate.rs`
- Modify `crates/voom-domain/src/lib.rs`
- Test in `crates/voom-domain/src/estimate.rs`

Define:

- `EstimateRun`, `FileEstimate`, `ActionEstimate`, `EstimateBreakdown`,
  `EstimateConfidence`, and `CostModelSample`.
- `EstimateInput { plans, workers, now }` named input for construction.
- `estimate_plans(input, model) -> EstimateRun`.

Rules:

- Skipped and empty plans cost zero and appear in counts.
- Container, metadata, subtitle, verify, and tag operations use fixed overhead.
- Transcode estimates use pixels-per-second when width, height, and duration
  are known; otherwise fall back to duration-based conservative cost.
- Output bytes use historical ratio p25/p50/p75 when available, otherwise
  default ratios by codec (`hevc`, `av1`, `h264`, audio codecs) and source
  bitrate.
- Uncertainty is high when fewer than five matching samples exist or required
  source dimensions/duration are missing.
- Wall time is `ceil(total_compute_ms / workers)` with at least one worker.
- A file is flagged as net-byte-loss when estimated bytes saved is negative.

### 2. Storage

Files:

- Modify `crates/voom-domain/src/storage.rs`
- Modify `crates/voom-domain/src/test_support.rs`
- Modify `plugins/sqlite-store/src/schema.rs`
- Create `plugins/sqlite-store/src/store/estimate_storage.rs`
- Modify `plugins/sqlite-store/src/store/mod.rs`
- Test in sqlite-store storage tests

Add `EstimateStorage` to `StorageTrait` with:

- `insert_estimate_run(&EstimateRun) -> Result<()>`
- `get_estimate_run(&Uuid) -> Result<Option<EstimateRun>>`
- `list_estimate_runs(limit: u32) -> Result<Vec<EstimateRun>>`
- `insert_cost_model_sample(&CostModelSample) -> Result<()>`
- `list_cost_model_samples(filters) -> Result<Vec<CostModelSample>>`

Add sqlite tables:

- `estimate_runs`
- `estimate_files`
- `estimate_actions`
- `cost_model_samples`

Persist JSON for nested action details where a normalized schema would add
premature complexity.

### 3. CLI surface

Files:

- Modify `crates/voom-cli/src/cli.rs`
- Create `crates/voom-cli/src/commands/estimate.rs`
- Modify `crates/voom-cli/src/commands/mod.rs`
- Modify `crates/voom-cli/src/commands/process/mod.rs`
- Modify `crates/voom-cli/src/commands/process/pipeline.rs`
- Test in `crates/voom-cli/tests/cli_tests.rs` and focused unit tests

Add:

- `ProcessArgs.estimate: bool`
- `ProcessArgs.estimate_only: bool`
- `ProcessArgs.confirm_savings: Option<ByteSize>`
- `Commands::Estimate(EstimateArgs)`
- `EstimateCommands::Run` for `voom estimate <path> --policy ...`
- `EstimateCommands::Calibrate` for `voom estimate calibrate`

Behavior:

- `--estimate` and `--estimate-only` imply dry-run and do not dispatch
  `PlanCreated`.
- `--estimate-only` is an alias and should be documented as such.
- `--confirm-savings` requires normal execution mode and skips files below the
  savings threshold after estimates are produced.
- `voom estimate` reuses the process discovery/introspection/policy planning
  path but exits after estimate persistence and rendering.
- Calibration records synthetic benchmark samples with the local host label,
  codec, backend, preset, pixels-per-second, size ratio, and sample count.

### 4. Rendering

Files:

- Create `crates/voom-cli/src/commands/estimate/render.rs`
- Test renderer snapshots or string assertions

Output must include:

- File count.
- Phase breakdown.
- Codec/backend breakdown for transcodes.
- Total wall time and compute time.
- Bytes in, bytes out, and bytes saved.
- Count of net-byte-loss files.
- Count of high-uncertainty files.
- Per-file details when `--verbose` is used.

Use existing size and duration formatting helpers where possible. Keep output
plain text for humans and add `--format json` only if an existing command pattern
already supports it cheaply.

### 5. Functional tests with generated corpus

Files:

- Modify `docs/functional-test-plan.md`
- Create `docs/functional-test-plan-preflight-estimates.md`
- Add focused functional tests under `crates/voom-cli/tests/functional_tests.rs`
  if the current harness supports process subcommands.

Generate corpus:

```sh
scripts/generate-test-corpus /tmp/voom-estimate-corpus \
  --profile coverage \
  --count 16 \
  --seed 205 \
  --duration 3
```

Functional assertions:

- `voom process /tmp/voom-estimate-corpus --policy docs/examples/preflight-archive.voom --estimate`
  prints estimate text and does not modify generated files.
- `voom estimate /tmp/voom-estimate-corpus --policy docs/examples/preflight-archive.voom`
  produces the same aggregate counts as process estimate mode.
- `voom process ... --confirm-savings 1GB --dry-run` flags or skips files below
  the per-file savings threshold without executing plans.
- A policy that transcodes low-bitrate small files flags at least one
  net-byte-loss prediction.
- Generated `manifest.json` is used to select representative source codecs,
  containers, HDR/non-HDR files, and tiny files.
- After estimate mode, sqlite contains an estimate run and file/action detail
  records, but no pending executable plans.

Accuracy checks:

- On the generated corpus, compare estimated bytes-out to actual dry-run model
  fixtures when no execution is available.
- On a small disposable execution subset, run the policy and assert total bytes
  saved is within 20% and wall time within 30% when the local host has at least
  five calibration samples. If insufficient samples exist, assert that high
  uncertainty is reported instead of failing accuracy.

### 6. User-facing documentation and examples

Files:

- Modify `docs/cli-reference.md`
- Modify `docs/INDEX.md`
- Create `docs/preflight-estimates.md`
- Create `docs/examples/preflight-archive.voom`
- Create `docs/examples/preflight-size-gate.voom`
- Create `docs/examples/tests/preflight-archive.test.json`
- Create `docs/examples/tests/preflight-size-gate.test.json`

Documentation must cover:

- What estimate mode does and does not execute.
- How `--estimate`, `--estimate-only`, `voom estimate`, and
  `--confirm-savings` differ.
- How confidence and uncertainty should be interpreted.
- How calibration improves local estimates.
- How to verify generated-corpus behavior with `scripts/generate-test-corpus`.
- Where what-if records are stored and how to list them if the CLI exposes that.

Examples must parse through the existing policy test harness and demonstrate:

- Archival transcode estimate with codec/backend attribution.
- Savings gate behavior for files likely to grow.

### 7. Web UI integration

Files:

- Add API endpoint in `plugins/web-server/src/api/policy.rs` or a new
  `estimate.rs` module.
- Modify router in `plugins/web-server/src/router.rs`.
- Modify policy/run template to show estimate summary before starting a run.
- Test in `plugins/web-server/tests/api_tests.rs` and template tests.

Behavior:

- Before starting a policy run, the UI can request an estimate for selected
  paths/policy.
- Show phase, time, bytes saved, uncertainty, and net-byte-loss warnings.
- Do not start execution until the user confirms.

If this slice is too large for the first PR, file a GitHub issue before merge
and immediately repeat the full plan/implementation/PR process for that issue.

### 8. Adversarial reviews

Create `docs/plans/issue-205-adversarial-review.md` before final PR review.

Review checks:

- Estimate mode never dispatches executor-triggering events.
- What-if records cannot be mistaken for pending executable plans.
- `--confirm-savings` cannot execute files that failed estimation.
- Small/low-bitrate files are flagged as net-byte-loss instead of being hidden
  in aggregate savings.
- Confidence text is present when sample counts are low.
- Calibration failures are actionable and do not corrupt existing samples.
- Functional tests inspect the generated corpus manifest and sqlite state, not
  only CLI text.
- Documentation does not promise unsupported web UI, JSON output, calibration
  accuracy, or estimate-vs-actual reports unless implemented.

## Commit Plan

Use small conventional commits:

1. `docs: plan preflight cost estimation`
2. `feat(domain): add preflight estimate model`
3. `feat(storage): persist estimate what-if records`
4. `feat(cli): render process estimate mode`
5. `feat(cli): add standalone estimate command`
6. `feat(cli): gate processing by estimated savings`
7. `feat(cli): record local estimate calibration samples`
8. `docs: document preflight estimate workflows`
9. `test: cover preflight estimates with generated corpus`
10. `feat(web): add policy run estimate preview`
11. `docs: add preflight estimate adversarial review`

## Verification Gates

Run before each relevant commit:

```sh
cargo fmt --all
cargo test -p voom-domain estimate
cargo test -p voom-cli cli_tests
cargo test -p sqlite-store estimate
```

Run before PR:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo test -p voom-cli --features functional -- --test-threads=4
scripts/generate-test-corpus /tmp/voom-estimate-corpus \
  --profile coverage \
  --count 16 \
  --seed 205 \
  --duration 3
```

## Deferred Work

None yet. Any implementation slice removed from this plan must be filed as a
GitHub issue before the PR merges, and that new issue must go through the same
plan, implementation, adversarial review, and PR process.
