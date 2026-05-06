# E2E Policy Audit Harness

Script-driven end-to-end test of VOOM applying any `.voom` policy to a media
library from a clean database. Captures rich pre/post state from both VOOM's
view (the SQLite DB) and ground truth (independent `ffprobe`), and emits diffs
the operator uses to judge policy correctness.

The harness is policy-agnostic: it does not parse `.voom` files and does not
encode any expected outcomes. **Pipeline correctness** (build, scan, jobs reach
a terminal state, no data loss, web endpoints up, no phase that produced 100%
job failures) is gated by the harness; **semantic correctness** ("did the
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
| `--no-probe` | (off) | Skip the independent `ffprobe` sweep (DB-only diffs) |

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
├── logs/                         one file per CLI invocation, plus *.rc exit-code sidecars
├── db-export/                    raw SQLite tables (post-process; consumed by build-summary)
├── reports/                      voom report --all, files, plans, jobs, events.json
├── web-smoke/                    curl statuses + body samples
├── diffs/
│   ├── files-summary.md          path-level (size/mtime/disappeared/new/common)
│   ├── codec-pivot.md            video-codec × container counts: pre vs post
│   ├── tracks-pivot.md           audio + subtitle pivots
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
