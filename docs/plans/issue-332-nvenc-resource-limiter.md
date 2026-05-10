# Issue 332: NVENC Resource Limiter

Issue: <https://github.com/randomparity/voom/issues/332>
Branch: `fix/issue-332-nvenc-limiter-docs`

## Plan

1. Confirm the process command builds a `PlanExecutionLimiter` from executor
   `parallel_limits`.
2. Confirm video transcode plans with `hw: auto` or missing per-action hardware
   use the active executor-level hardware backend as their default resource.
3. Lower the default NVENC limit to a conservative value for CUDA decode+encode
   workloads.
4. Document user-facing configuration and add a policy example plus functional
   test steps using `scripts/generate-test-corpus`.

## Implementation

The limiter implementation was already present on `main`: the ffmpeg executor
advertises `hw:nvenc`, the process command passes the best detected hardware
backend as the default resource, and job-manager tests cover explicit NVENC,
`hw: auto`, missing `hw`, and `hw: none`.

This issue lowers the default NVENC parallelism from `4` to `2` and documents
`nvenc_max_parallel` so high-worker e2e runs can be tuned without code changes.

## Functional Test

See `docs/functional-test-plan-issue-332.md`. It generates a small H.264 corpus
and runs `docs/examples/hw-nvenc-hevc.voom` with `--workers` above
`nvenc_max_parallel`.

## Acceptance Criteria Review

Reproduce with a small batch and high `--workers` value:

- Covered by the functional test plan using `scripts/generate-test-corpus` and
  `--workers 8`.

Confirm NVENC plans acquire the `hw:nvenc` semaphore even when the policy uses
auto/default hardware:

- Existing tests cover default-resource acquisition in `job-manager` and the
  process pipeline.

Add test coverage for limiter classification of HEVC NVENC transcode plans:

- Existing `PlanParallelResource` and pipeline limiter tests cover explicit
  `hw: nvenc`, `hw: auto`, missing `hw`, and `hw: none`.

Either lower the default NVENC limit or make it configurable/documented enough
that e2e can run without mass OOM failures:

- Default lowered to `2`.
- `nvenc_max_parallel` is documented in config comments and
  `docs/hardware-transcoding.md`.

## Adversarial Review

- Risk: lower default underuses large GPUs. Mitigation: users can raise
  `nvenc_max_parallel` after a representative e2e run passes.
- Risk: file workers may still be high. Mitigation: the limiter sits at plan
  dispatch, so extra workers wait before ffmpeg creates CUDA contexts.
- Risk: non-NVENC hardware could need similar tuning. Mitigation: QSV/VA-API
  resource names already flow through the same limiter; per-backend defaults
  can be added if e2e evidence shows a problem.
