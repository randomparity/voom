# E2E Containerize Test Harness

Script-driven end-to-end test of VOOM applying `01-containerize.voom`
to the series library at `/mnt/raid0/media/series` from a clean database.

## Usage

```bash
scripts/e2e-containerize/run.sh \
    --library /mnt/raid0/media/series \
    --policy ~/.config/voom/policies/01-containerize.voom \
    --run-dir ~/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)
```

All flags have defaults matching the values above; passing none runs
against the canonical configuration.

## Pre-conditions

- `~/.config/voom/voom.db` must not exist
- `~/.config/voom/plugins/` must not exist
- `ffmpeg`, `ffprobe`, `mkvmerge`, `sqlite3`, `jq`, `curl` on PATH
- The library path must be readable; libraries with active writers
  during a run will produce confusing diffs

## Run-dir layout

| Path | Contents |
|------|----------|
| `pre/`, `post/` | library snapshots (manifest + tallies) |
| `logs/` | one file per CLI call, plus `process.log` |
| `db-export/` | `schema.sql` + per-table `.tsv` |
| `reports/` | `voom report` / `history` / `files` / `plans` / `events` / `jobs` |
| `web-smoke/` | curl statuses + body samples |
| `diff-summary.md` | path-level pre→post diff |
| `summary.md` | PASS/WARN/FAIL verdict + anomaly section |

## Interpreting `summary.md`

- **PASS** — every hard criterion in the spec was met.
- **WARN** — hard criteria met but soft criteria flagged (e.g. some
  AVI files failed to remux, or duration outliers detected).
- **FAIL** — at least one hard criterion violated. The summary names
  the specific criterion and the offending evidence.

See `docs/superpowers/specs/2026-05-04-e2e-containerize-test-design.md`
for the full criteria.
