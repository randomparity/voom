# Hardware Transcoding

VOOM can route video transcodes through hardware encoders such as NVENC, QSV,
VA-API, and VideoToolbox. Hardware jobs still run inside the normal
`voom process --workers` file worker pool, but executor-advertised hardware
resources add a second plan-level limit so many file workers do not create too
many GPU contexts at once.

## NVENC Concurrency

When `[plugin.ffmpeg-executor] hw_accel = "nvenc"` is active, the ffmpeg
executor advertises a `hw:nvenc` plan limit. `transcode video` plans with
`hw: nvenc`, `hw: auto`, or no per-action `hw` setting acquire that limiter
before executor dispatch. `hw: none` stays software and does not consume an
NVENC permit.

The default NVENC limit is `2` concurrent plans per process. This conservative
default is intended for decode+encode workloads where each ffmpeg process may
create CUDA decoder and encoder contexts.

Tune the limit in `~/.config/voom/config.toml`:

```toml
[plugin.ffmpeg-executor]
hw_accel = "nvenc"
gpu_device = "0"
nvenc_max_parallel = 2
```

Use `1` for small GPUs or when CUDA/NVENC out-of-memory errors appear. Increase
only after a representative e2e run completes without GPU allocation failures.
The file worker count can still be higher; extra hardware transcode plans wait
on the NVENC permit while software-only work continues.

## Functional Check

Generate a small hardware-transcode corpus:

```sh
scripts/generate-test-corpus /tmp/voom-hw-transcode \
  --profile coverage \
  --only basic-h264-aac,letterbox-h264 \
  --duration 2 \
  --seed 332
```

Run with more file workers than the NVENC limit:

```sh
voom process /tmp/voom-hw-transcode \
  --policy docs/examples/hw-nvenc-hevc.voom \
  --on-error continue \
  --workers 8
```

Expected results:

- no CUDA/NVENC out-of-memory failures in the phase breakdown
- no more than `nvenc_max_parallel` ffmpeg NVENC plans executing at once
- `voom report errors --session <session>` is empty or contains only unrelated
  file-specific failures
