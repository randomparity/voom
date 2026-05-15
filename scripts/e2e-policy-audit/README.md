# E2E Policy Audit Harness

Script-driven end-to-end test of VOOM applying any `.voom` policy to a media
library from a clean database. Captures rich pre/post state from both VOOM's
view (the SQLite DB) and ground truth (independent `ffprobe`), and emits diffs
the operator uses to judge policy correctness.

The harness is policy-agnostic: it does not parse `.voom` files and does not
encode any expected outcomes. **Pipeline correctness** (build, scan, jobs reach
a terminal state, no data loss, web endpoints up, no failed plans in any phase)
is gated by the harness; **semantic correctness** ("did the
policy do what I wanted") is the operator's judgment from the diffs.

## Usage

```bash
scripts/e2e-policy-audit/run.sh \
    --library /mnt/raid0/media/series \
    --policy ~/.config/voom/policies/02-hw-transcode-hevc.voom \
    --run-dir ~/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)-hw-transcode-hevc
```

All flags have defaults (see `run.sh --help`):

| Flag | Default | Meaning |
|------|---------|---------|
| `--library` | `/mnt/raid0/media/series` | Library root to scan |
| `--policy` | `~/.config/voom/policies/02-hw-transcode-hevc.voom` | Policy to apply |
| `--run-dir` | `~/voom-e2e-runs/<ts>-<policy-stem>` | Where artifacts land |
| `--probe-workers` | `8` | Parallelism for `ffprobe` sweep |
| `--no-build` | (off) | Skip `cargo build --release` |
| `--no-web` | (off) | Skip the `voom serve` smoke-test |
| `--no-probe` | (off) | Skip the independent `ffprobe` sweep and ffprobe-backed metadata diffs |

### Long-run sidecar intervals

The long-run sidecars default to issue #402's production cadence:

| Variable | Default | Meaning |
|---|---:|---|
| `VOOM_E2E_DMON_INTERVAL_SECONDS` | `30` | GPU power/clock/utilization sample interval |
| `VOOM_E2E_DB_CHECKPOINT_INTERVAL_SECONDS` | `21600` | DB checkpoint interval, six hours |
| `VOOM_E2E_WATCHDOG_INTERVAL_SECONDS` | `600` | Jobs-list poll interval |
| `VOOM_E2E_WATCHDOG_STUCK_POLLS` | `12` | Consecutive unchanged polls before `USR1` |

After a failed run, inspect `summary.md` first. For failed-plan runs,
`diffs/failure-clusters.md` groups representative problem files; to materialize
a small library containing them:

```bash
~/voom-e2e-runs/<run>/repro/copy-repro-set.sh /tmp/voom-repro-library
```

Then rerun the harness against `/tmp/voom-repro-library` with the same policy.

To rerun only files that had failed plans:

```bash
~/voom-e2e-runs/<run>/repro/replay.sh
BUILD=target/release/voom POLICY=/path/to/policy.voom ~/voom-e2e-runs/<run>/repro/replay.sh
```

`replay.sh` defaults to `command -v voom` and the policy snapshot copied into
`env/policy.voom`.

## Pre-conditions

- `~/.config/voom/voom.db` must NOT exist.
- `~/.config/voom/plugins/` must NOT exist.
- These tools must be on PATH: `ffmpeg`, `ffprobe`, `mkvmerge`, `sqlite3`, `jq`,
  `curl`, `find`, `xargs`, `awk`, `python3`.
- The library path must be readable.
- Libraries with active writers during a run will produce confusing diffs.

## Run-dir layout

```
~/voom-e2e-runs/<YYYY-MM-DD-HHMMSS>-<policy-stem>/
├── manifest.json                 run metadata + per-stage timings
├── pre/, post/                   library snapshots (find manifest + ffprobe NDJSON + DB NDJSON)
│   └── voom-db-tables/           raw SQLite export (per-table TSV) post-scan / post-process
├── runtime/                      5-minute host state samples during voom process
│   └── nvidia-dmon.csv           nvidia-smi dmon power/clock/utilization timeseries
├── env/                          tool versions, GPU state, policy copy, redacted config
│   ├── version.json              structured VOOM build/version metadata
│   ├── journal.log               host journal captured after voom process
│   ├── dmesg.log                 kernel ring buffer captured after voom process
│   ├── dnf-history.txt           recent package manager history
│   └── rpm-recently-changed.txt  recently changed installed RPMs
├── logs/                         one file per CLI invocation, plus *.rc exit-code sidecars
│   ├── plugin-errors/            compact repeated plugin.error signature logs
│   ├── env-check/                hourly voom env check snapshots during process
│   └── watchdog.log              forward-progress watchdog polls and timeout reason
├── db-export/                    raw SQLite tables (post-process; consumed by build-summary)
│   └── checkpoint-NNNN/          periodic raw SQLite table exports during process
├── reports/                      voom report --all, files, plans, jobs, events
│   ├── scan.json                 structured `voom scan --format json` summary
│   ├── process.json              structured `voom process --format json` summary
│   ├── jobs.json                 structured `voom jobs list --format json` output
│   ├── report.json               structured `voom report --all --format json` output
│   ├── policy-validate.json      structured policy validation result
│   ├── env-check.json            structured environment check result
│   ├── events.json               raw `voom events -f json` capture
│   └── events-deduped.json       raw events with repeated plugin errors compacted
├── repro/                        problem-file lists + replay/copy scripts
│   ├── replay.sh                 rerun failed-plan files against BUILD/POLICY
│   └── copy-repro-set.sh         copy representative problem files to a small library
├── web-smoke/                    statuses + body samples + content assertions
├── diffs/
│   ├── db-growth.tsv             row count per exported table at each checkpoint
│   ├── plugin-error-summary.md   repeated plugin.error signatures by plugin
│   ├── plan-preview-vs-executed.tsv/.md  planned vs executed phase/action/skip diff
│   ├── deprecations.md           `warning:` lines from logs/*.log
│   ├── failure-timeline.md       failed plans bucketed by hour and cause
│   ├── runtime-timeline.md       summarized runtime host state changes
│   ├── env-check-timeline.md     summarized env check state changes
│   ├── files-summary.md          path-level (size/mtime/disappeared/new/common)
│   ├── codec-pivot.md            video-codec × container counts: pre vs post
│   ├── tracks-pivot.md           audio + subtitle pivots
│   ├── failure-clusters.tsv/.md   failed plans grouped by signature/source shape
│   ├── *-summary.tsv/.md          high-level diff class counts
│   ├── db-vs-ffprobe-pre.tsv     VOOM introspection accuracy at scan time
│   ├── db-vs-ffprobe-post.tsv    VOOM re-introspection accuracy after process
│   ├── voom-db-pre-vs-post.tsv   what VOOM thinks changed
│   └── ffprobe-pre-vs-post.tsv   what actually changed on disk
└── summary.md                    PASS/WARN/FAIL verdict + anomaly section + links
```

## Interpreting `summary.md`

- **PASS** — every hard criterion was met (no pipeline-level breakage).
- **WARN** — hard criteria met, but soft criteria flagged something worth a look.
- **FAIL** — at least one hard criterion violated. The summary names the
  specific criterion and the offending evidence.

The harness does **not** judge whether your policy did the *right thing*. To
do that, read `diffs/codec-pivot.md`, `diffs/tracks-pivot.md`, and the four
`*.tsv` ndjson diffs — they describe what changed without prescribing what
*should* have changed.

For large runs, start with the aggregate views:

- `diffs/failure-timeline.md` shows whether failures are clustered in time or
  spread across the run.
- `diffs/failure-clusters.md` groups failed plans by phase, error signature,
  exit code, source container, and source video codec.
- `diffs/plan-preview-vs-executed.md` highlights drift between `--plan-only`
  preview plans and the plans persisted after execution.
- `diffs/deprecations.md` lists `warning:` lines emitted by CLI invocations.
- `diffs/plugin-error-summary.md` compresses repeated plugin error payloads and
  points to per-plugin logs under `logs/plugin-errors/`.
- `diffs/db-vs-ffprobe-post-summary.md` groups post-run introspection
  divergences by stable signatures such as subtitle default drift or attachment
  promotion.
- `diffs/ffprobe-pre-vs-post-summary.md` groups actual on-disk metadata changes
  independently of VOOM's DB view.
- `repro/minimal-covering-set.tsv` picks a capped set of representative files
  per failure/diff signature for faster follow-up runs.

## Pre-Release Audit Gates

Pre-release audit runs should have:

- zero rows in `diffs/plan-preview-vs-executed.tsv`, unless each divergence is
  intentionally explained in release notes;
- zero warnings in `diffs/deprecations.md`.

Set `VOOM_E2E_FAIL_ON_DEPRECATIONS=1` to make warning lines a hard summary
failure instead of a soft warning.

## Canonical metadata comparison

The `db-vs-ffprobe-*` diffs compare two canonical NDJSON views: VOOM's SQLite
metadata export and a fresh `ffprobe` parse. The harness normalizes known
serialization artifacts at those boundaries, including full-precision SQLite
REAL rendering for frame rates and literal quote wrappers around common audio
channel title labels such as `"2.0"` and `"5.1"`. Do not add these fields to
`lib/ndjson-ignore.txt`; that hides real metadata regressions.

## Tests

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Runs every diff/pivot script against a hand-crafted fixture and asserts the
output matches `tests/expected/`. Add new scenarios under
`tests/fixtures/<scenario>/` and corresponding expected outputs under
`tests/expected/<scenario>/`.

See `docs/superpowers/specs/2026-05-05-e2e-policy-audit-design.md` for the
full design.
