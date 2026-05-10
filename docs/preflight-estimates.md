# Pre-flight Estimates

Pre-flight estimates evaluate policy plans without executing them. They help
answer whether a run is likely to save space, how long it may take, and which
files have weak cost-model confidence.

## Commands

Estimate a normal process run:

```sh
voom process /media/movies --policy docs/examples/preflight-archive.voom --estimate
```

`--estimate-only` is an alias for `--estimate`.

Use the standalone command when no process flags are needed:

```sh
voom estimate /media/movies --policy docs/examples/preflight-archive.voom --workers 4
```

Seed local calibration samples:

```sh
voom estimate calibrate
```

Without extra flags, calibration records conservative codec/backend samples in
the VOOM database. To measure this machine against generated fixtures, create a
corpus and pass it to calibration:

```sh
scripts/generate-test-corpus /tmp/voom-estimate-corpus \
  --profile smoke \
  --count 0 \
  --seed 310 \
  --duration 1

voom estimate calibrate \
  --benchmark-corpus /tmp/voom-estimate-corpus \
  --max-fixtures 3
```

Corpus-backed calibration reads `manifest.json`, transcodes the first generated
video fixtures through a bounded HEVC software benchmark, and records measured
pixels/second plus output-size ratios. It prints an estimate-vs-actual summary
using holdout fixtures, so each validation fixture is estimated from samples
measured from other fixtures. Run with at least two fixtures; a single fixture is
recorded for calibration but is not enough to report holdout accuracy. The
estimator uses matching samples before falling back to built-in defaults.

## Savings Gate

Require estimated per-file savings before execution:

```sh
voom process /media/movies \
  --policy docs/examples/preflight-size-gate.voom \
  --confirm-savings 1GB
```

Files below the threshold are skipped before `PlanCreated` is dispatched, so
executor plugins do not run for those plans.

## Web UI

The Web UI exposes persisted estimate records at `/estimates`. Open a record
with **Review**, enter the target media path and policy path, then choose
**Confirm Run**. The UI reloads the persisted estimate and shows the final
confirmation summary before dispatching any processing work.

Use **Cancel** from either dialog to leave the estimate as a what-if record.
No `PlanCreated` events are dispatched until the final **Start Run** action is
confirmed.

## Confidence

High uncertainty means the estimator had fewer than five matching samples or
was missing source dimensions/duration. Treat those estimates as directional.
Run `voom estimate calibrate`, or compare future estimates against completed
runs, to improve the local model.

## Generated Corpus Check

Create test media:

```sh
scripts/generate-test-corpus /tmp/voom-estimate-corpus \
  --profile coverage \
  --count 16 \
  --seed 205 \
  --duration 3
```

Then estimate:

```sh
voom process /tmp/voom-estimate-corpus \
  --policy docs/examples/preflight-archive.voom \
  --estimate
```

Inspect `/tmp/voom-estimate-corpus/manifest.json` to pick fixtures by codec,
container, and generated traits when writing functional tests.
