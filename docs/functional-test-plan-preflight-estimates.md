# Functional Test Plan: Pre-flight Estimates

Generate a synthetic corpus:

```sh
scripts/generate-test-corpus /tmp/voom-estimate-corpus \
  --profile coverage \
  --count 16 \
  --seed 205 \
  --duration 3
```

Estimate mode:

```sh
voom process /tmp/voom-estimate-corpus \
  --policy docs/examples/preflight-archive.voom \
  --estimate
```

Expected:

- Output includes total wall time, compute time, bytes in/out/saved, high
  uncertainty count, and net-byte-loss count.
- Generated files are not modified.
- The sqlite database contains an estimate run and file/action detail JSON.
- No pending executable plans are created by the estimate-only run.

Standalone command:

```sh
voom estimate /tmp/voom-estimate-corpus \
  --policy docs/examples/preflight-archive.voom \
  --workers 4
```

Expected: aggregate counts match `voom process --estimate` for the same corpus.

Savings gate:

```sh
voom process /tmp/voom-estimate-corpus \
  --policy docs/examples/preflight-size-gate.voom \
  --confirm-savings 1GB \
  --dry-run
```

Expected: files below the threshold are skipped before executor dispatch.

Calibration:

```sh
voom estimate calibrate
```

Expected: the command records codec/backend cost-model samples, and later
estimate runs use matching samples before built-in defaults.
