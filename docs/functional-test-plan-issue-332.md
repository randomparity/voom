# Functional Test Plan: NVENC Resource Limiting

Issue: <https://github.com/randomparity/voom/issues/332>

## Goal

Verify that hardware HEVC transcode runs respect the plan-level `hw:nvenc`
limiter even when the file worker count is higher than the configured NVENC
parallelism.

## Corpus Setup

```sh
scripts/generate-test-corpus /tmp/voom-issue-332-corpus \
  --profile coverage \
  --only basic-h264-aac,letterbox-h264 \
  --duration 2 \
  --seed 332
```

Use `docs/examples/hw-nvenc-hevc.voom` as the policy.

## Configuration

Start with the conservative default or set it explicitly:

```toml
[plugin.ffmpeg-executor]
hw_accel = "nvenc"
gpu_device = "0"
nvenc_max_parallel = 2
```

## Execution

Run with file workers above the NVENC limit:

```sh
voom process /tmp/voom-issue-332-corpus \
  --policy docs/examples/hw-nvenc-hevc.voom \
  --on-error continue \
  --workers 8
```

## Assertions

1. The run does not produce CUDA/NVENC out-of-memory failures.
2. The phase breakdown does not show mass `transcode-video` errors.
3. `hw: nvenc`, `hw: auto`, and default-hardware video transcode plans acquire
   the same `hw:nvenc` limiter when NVENC is the active backend.
4. Raising `--workers` above `nvenc_max_parallel` increases queued file work but
   does not increase concurrent NVENC ffmpeg executions beyond the configured
   limit.

## Adversarial Review

- A high `--workers` value should not bypass the hardware limiter.
- `hw: none` must remain software and must not consume an NVENC permit.
- The default limit must be conservative enough for CUDA decode+encode
  workloads; users can raise `nvenc_max_parallel` after validating their GPU.
