# E2E Containerize Test — Design

**Date:** 2026-05-04
**Author:** David Christensen (with Claude)
**Status:** Superseded
**Superseded by:** [`2026-05-05-e2e-policy-audit-design.md`](2026-05-05-e2e-policy-audit-design.md)

## Goal

Run a full-lifecycle end-to-end test of VOOM against a real-world 27 TB / 23,705-file series library, applying the `01-containerize.voom` policy (remux all containers to MKV) starting from a clean database. Capture artifacts and a before/after comparison sufficient to validate correctness and surface anomalies.

## Scope

- Library: `/mnt/raid0/media/series` (23,417 MKV / 77 MP4 / 211 AVI; 27 TB).
- Policy: `~/.config/voom/policies/01-containerize.voom` — minimal policy, single `containerize` phase targeting MKV, with `keep_backups: true`.
- Database: starts absent under the default path (`~/.config/voom/`); all VOOM state created fresh.
- Build: release binary (`cargo build --release --workspace`).
- Execution time is unbounded — the run is allowed to complete naturally.

## Approach (chosen: hybrid C)

Script-driven CLI E2E run with a brief `voom serve` smoke-test at the end:

- A bash driver under `~/voom-e2e-runs/<timestamp>/run.sh` orchestrates everything.
- All output (logs, snapshots, DB exports, reports, summary) is captured in the same run dir for offline review and run-to-run diffing.
- After the batch completes, `voom serve` is started briefly (≤ 2 min) to verify the web/SSE surface reflects the run, then shut down.

## Run-directory layout

```
~/voom-e2e-runs/<YYYY-MM-DD-HHMMSS>/
├── run.sh                  # the driver (committed to repo as well)
├── pre/                    # library snapshot before run
│   ├── library-manifest.tsv
│   ├── ext-tally.txt
│   ├── size-totals.txt
│   └── non-mkv-files.txt
├── post/                   # library snapshot after run (same shape as pre/)
├── logs/                   # one file per CLI invocation, plus process.log
├── db-export/              # schema.sql + per-table TSV dumps
├── reports/                # voom report / history / files / plans / events / jobs
├── web-smoke/              # curl outputs from `voom serve` smoke-test
├── diff-summary.md         # generated comparison between pre/ and post/
└── summary.md              # PASS/WARN/FAIL verdict + anomaly section
```

## Pipeline sequence

### Pre-flight
1. Assert clean state — `~/.config/voom/voom.db` and `~/.config/voom/plugins/` must not exist; abort if either is present.
2. `cargo build --release --workspace` (single build; everything below uses `target/release/voom`).
3. `voom --version`, `voom doctor`, `voom health` — log tool versions and config sanity.
4. `voom policy validate ~/.config/voom/policies/01-containerize.voom`.
5. Capture pre-snapshot of the library (see Snapshot strategy).

### Discovery + introspection
6. `voom scan /mnt/raid0/media/series` — discovery; introspector populates DB.
7. `voom files` — dump the discovered inventory.
8. `voom inspect <a few sample paths>` — spot-check single-file metadata.

### Planning + execution
9. `voom plans --policy 01-containerize` — preview planned actions.
10. `voom process --policy 01-containerize` — the long run, output `tee`'d to `logs/process.log`. Unbounded duration.
11. `voom jobs` — final job table.

### Post-run inspection
12. `voom events --since <run-start>` → `reports/events.jsonl`.
13. `voom report` → `reports/report.txt`.
14. `voom history` → `reports/history.txt`.
15. `voom db dump` (or equivalent) → `db-export/`.
16. Capture post-snapshot of the library.

### Web UI smoke-test
17. `voom serve --port 18080` (background); curl `/`, `/api/files`, `/api/jobs`, `/api/events`; capture HTTP statuses + body samples; shut down.

Exact subcommand flags will be confirmed via `voom <cmd> --help` during execution; the design does not assume specific spellings.

## Snapshot strategy

Full hashing of 27 TB is impractical. Each snapshot captures:

- **`library-manifest.tsv`** — one row per video file: `path \t size \t mtime \t extension`. Generated via `find` + `stat`.
- **`ext-tally.txt`** — counts per extension (`mkv`, `mp4`, `avi`, `m4v`, `mov`, `ts`, `webm`, `bak`).
- **`size-totals.txt`** — total bytes per extension and grand total.
- **`non-mkv-files.txt`** — full path list of every non-MKV video (the high-signal set; ~288 paths). This is the population the policy will actually transform.

## Before/after comparison (`diff-summary.md`)

- Row-level join on `path` between pre and post manifests → each file classified as `unchanged`, `mtime-changed`, `size-changed`, `disappeared`, `new`.
- Per-extension count delta (e.g. `avi: 211 → 0`, `mkv: 23417 → 23705`, `bak: 0 → ~288`).
- Total-bytes delta, broken down by extension.
- Spot-check: 5 random files from each of `{mkv-unchanged, mp4→mkv-converted, avi→mkv-converted}` are re-probed with `ffprobe`; container/codec/track summary logged.
- `keep_backups` invariant: every pre non-MKV must have a sibling `.bak` in the post snapshot and a sibling `.mkv` of similar duration.

## Success criteria

### Hard (FAIL on violation)
- `voom doctor` and `voom health` exit 0 pre-run.
- `voom policy validate` exits 0.
- `voom scan` discovers ≥ 23,705 video files (matches pre-snapshot count).
- `voom process` exits 0; or if non-zero, every input is accounted for in the jobs table (no silent drops).
- Every job in the final jobs table is in a terminal state (no stuck `running`/`pending`).
- No row-level `disappeared` file lacking a corresponding `.bak` (no data loss).
- The 23,417 pre-existing MKVs are byte-identical post-run (size + mtime unchanged) — pass-through must be a no-op.
- Web smoke-test: `/`, `/api/files`, `/api/jobs`, `/api/events` return 2xx and the file count via the API matches the DB count.

### Soft (WARN)
- Any AVI/MP4 that did not produce a sibling `.mkv` (likely codec-incompatible — expected for some AVIs).
- Any job with status `failed` — captured with file path + error string into a "failures" section.
- Event-log gaps (e.g. `FileDiscovered` without a matching `FileIntrospected`).
- Wallclock duration outliers (per-file process time > 3σ from the mean).
- New on-disk artifacts that aren't `.mkv` or `.bak` (unexpected outputs).

### Anomaly section in `summary.md`
- Top 10 longest-running jobs.
- Full list of failed jobs with error messages.
- Files counted by `scan` but absent from `inspect` results.
- Disk-usage delta (expected: roughly +size of `.bak` copies of the ~288 non-MKVs minus original-source replacements; net positive, bounded).

## Out of scope

- Hashing 27 TB of files for byte-level comparison.
- Restoring or rolling back the library after the run — the user has a backup that can restore.
- Per-plugin internals validation; this is a black-box system test.
- Performance benchmarking beyond duration outliers; throughput tuning is not the goal.
- WASM plugin loading — only native plugins are exercised by the default policy.

## Risks & mitigations

- **AVI codec incompatibility** — some AVIs (e.g. MPEG-4 ASP / DivX) may not remux cleanly into MKV without transcoding. Treated as a soft WARN; failed remuxes are surfaced individually rather than failing the whole run.
- **Long runtime** — multi-day execution likely. The driver writes a status file each phase so progress is visible without tailing logs.
- **Disk space** — `keep_backups: true` doubles the on-disk footprint of converted files. Only ~288 files are converted, so total impact is bounded; pre-flight does not currently gate on free space (can be added if needed).
- **Side effects on user config** — chosen approach (A: real config dir) means future runs require manually wiping `~/.config/voom/voom.db` and `~/.config/voom/plugins/` first. Pre-flight check enforces this gate.
