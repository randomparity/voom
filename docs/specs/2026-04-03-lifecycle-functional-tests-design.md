# Lifecycle Functional Tests & Corpus Generator Scaling

**Date:** 2026-04-03
**Status:** Approved
**Branch:** feat/file-lifecycle-tracking

## Problem

The file lifecycle tracking feature (soft-delete, transitions, move detection,
crash recovery) has 5 basic functional tests covering the happy paths. Edge
cases, multi-root scenarios, process transition recording, crash recovery, and
scale testing are not covered. The corpus generator produces a fixed set of 11
files, insufficient for scale testing and unable to produce corrupt files for
error-path validation.

## Goals

1. Extend `generate-test-corpus` to produce arbitrary numbers of unique files
   with realistic names, varied properties, and optional corruption.
2. Add functional tests covering all lifecycle tracking scenarios: multi-root
   reconciliation, external modification detection, reactivation, process
   transition recording, crash recovery, and statistics filtering.
3. Support configurable scale (file count, iteration count) for stress testing
   outside CI.

## Part 1: Corpus Generator Extensions

### New flags

| Flag | Default | Description |
|------|---------|-------------|
| `--count N` | 0 | Generate N additional random files after manifest files |
| `--corrupt N` or `--corrupt N%` | 0 | Corrupt N files (or N% of `--count`) from the random set |
| `--duration-range MIN-MAX` | `1-5` | Duration range in seconds for random files |

Existing flags are unchanged:
- `--duration` applies to manifest files only (backward compatible)
- `--seed` seeds all RNG (manifest metadata + random generation + corruption selection)
- `--only` and `--skip` apply to manifest files only; random files always generated when `--count` is set

### Random file generation

Each random file spec is built by combining:

**Container:** MKV or MP4 (equal probability).

**Video codec:** `libx264` or `libx265` (equal probability).

**Resolution:** Random from `[640x360, 854x480, 1280x720, 1920x1080, 3840x2160]`.

**FPS:** Random from `[24, 25, 30]`.

**Duration:** Random integer in `--duration-range` (default 1-5 seconds).

**Audio tracks:** 1-3 tracks. Each track randomly selects:
- Codec: `aac`, `ac3`, `mp3`, or `flac`
- Channels: 1, 2, or 6
- Language: random from `LANG_POOL`
- Title: random from `AUDIO_TITLE_POOL`

**Subtitle tracks:** 0-2 tracks. Each track randomly selects:
- Format: `srt` or `ass`
- Language: random from `LANG_POOL`
- Forced: 10% chance

**Metadata:** Random title, encoder, creation_time, handler_name from existing
pools. Track-level languages and titles also randomized.

**Uniqueness:** Each file gets a unique name (collision-checked) and a unique
content signature (duration + codec + resolution + fps + container + track
layout). If a signature collision occurs, the duration is bumped to force a
distinct content hash.

### Realistic naming

File names are assembled from pools to simulate a real media library. Each name
randomly selects a pattern:

**Patterns:**
1. Movie style: `Title.Year.Quality.Source.codec-Group.ext`
   Example: `Crimson.Meridian.2024.1080p.BluRay.x264-SPARKS.mkv`
2. TV style: `Show.S01E03.Episode.Title.Quality.Source.ext`
   Example: `Orbital.Descent.S02E07.The.Last.Stand.720p.WEB-DL.mp4`
3. Simple title: `Title (Year).ext`
   Example: `The Silent Echo (2023).mkv`
4. Foreign/unicode: `Title.Quality.ext` with non-ASCII characters
   Example: `Cafe.Paramo.2024.1080p.mkv` or `Uber.den.Wolken.S01E05.720p.mp4`

**Title pools (expanded):**
- English: `"The Silent Echo"`, `"Crimson Meridian"`, `"Neon Reef"`,
  `"Orbital Descent"`, `"Dustwalker"`, `"Iron Chorus"`, `"Abyssal Reach"`,
  `"The Vanishing Gradient"`, `"Solstice Run"`, `"Waypoint Zero"`,
  `"The Last Cartographer"`, `"Parallax"`, `"Black Meridian"`,
  `"The Obsidian Gate"`, `"Phantom Corridor"`, `"Static Bloom"`,
  `"Voidrunner"`, `"The Copper Key"`, `"Fractured Light"`,
  `"Signal Drift"`, `"Terminal Velocity"`, `"Hollow Earth"`
- Foreign: `"Cafe Paramo"`, `"Uber den Wolken"`, `"La Derniere Nuit"`,
  `"El Sueno Eterno"`, `"Der letzte Horizont"`, `"Le Cercle Rouge"`,
  `"Nocturne d'Argent"`, `"Sous les etoiles"`, `"Die Stille Welt"`

**Edge character pool** (mixed into ~20% of names):
- Parentheses: `(Extended Cut)`, `(Director's Cut)`, `(Theatrical)`
- Brackets: `[REPACK]`, `[PROPER]`, `[MULTI]`
- Apostrophes/quotes: `It's`, `O'Brien`, `"Special"`
- Accented characters: `e with accent`, `u with umlaut`, `n with tilde`
- Long names: some titles padded to 200+ characters

**Separator styles:** Dots (60%), spaces (15%), dashes (15%), underscores (10%).

**Quality tags:** `480p`, `720p`, `1080p`, `2160p`, `4K`

**Source tags:** `BluRay`, `WEB-DL`, `HDTV`, `REMUX`, `BDRip`, `DVDRip`,
`WEBRip`, `AMZN`, `NF`

**Group tags:** `SPARKS`, `YTS.MX`, `RARBG`, `FGT`, `EVO`, `FLUX`, `NOGRP`

### Corruption

Corruption is applied after ffmpeg generates the valid file. The generator
selects `--corrupt N` random files from the `--count` set and applies one
randomly-chosen corruption type per file:

| Type | Description |
|------|-------------|
| Truncated | File cut to 10-80% of original size |
| Header damage | First 64-512 bytes overwritten with random data |
| Mid-stream | Random 1KB-4KB block in middle 50% of file replaced with random bytes |
| Zero-length | File truncated to 0 bytes |
| Wrong extension | Extension swapped (`.mkv` to `.mp4` or vice versa) |

**Constraints:**
- Manifest files are never corrupted
- `--corrupt N` requires `--count >= N`; error if `--corrupt` is set without `--count`
- `--corrupt N%` computes N as percentage of `--count`, rounded up; 0% is a no-op

**Output:** The generator prints which files were corrupted and how, for
debugging. Seed makes corruption selection reproducible.

### Implementation approach

Random file generation reuses the existing `build_ffmpeg_cmd()` function. A new
`build_random_specs(count, seed, duration_range)` function produces a list of
spec dicts in the same format as `build_manifest()`. These specs are appended
to the manifest specs and processed through the existing pipeline.

Corruption is a post-processing step: after all files are generated, the
corruption function iterates the selected files and mutates them on disk.

## Part 2: Functional Test Design

### Test infrastructure

**Scale presets:** Constants at the top of the test module, overridable via
`VOOM_TEST_SCALE` environment variable:

| Preset | Files per root | Roots | Lifecycle iterations | Corrupt files |
|--------|---------------|-------|---------------------|---------------|
| `small` (default) | 3 | 2 | 3 | 1 |
| `medium` | 10 | 3 | 5 | 3 |
| `large` | full corpus | 4 | 10 | 5 |

Tests use `TestEnv` and the existing shared corpus. For multi-root tests, files
are distributed across N root directories within the test environment's tempdir.

**Helper: `populate_multi_root`** — distributes files from the corpus across N
root directories (round-robin or configurable). Returns a `Vec<PathBuf>` of
root paths.

**Helper: `query_db`** — wraps common SQLite queries (count by status, count
transitions by source, get file by path, etc.) to reduce boilerplate.

### Test categories

All tests go in a new `test_lifecycle_advanced` module in
`functional_tests.rs`.

#### A. Multi-root scan reconciliation

**A1. `scan_multi_root_independent`**
Scan two roots with different files. Verify each root's files are tracked
independently. Delete a file from root A, rescan both roots. Root A file is
missing, root B files remain active.

**A2. `scan_subset_doesnt_affect_unscanned`**
Scan root A and root B. Then scan only root A. Files in root B remain active
(not marked missing).

**A3. `scan_file_moved_within_root`**
Scan root A. Move a file to a subdirectory within root A. Rescan. Verify move
detected (same UUID, new path, `detected_move` transition).

**A4. `scan_file_moved_between_roots`**
Scan root A. Move file from root A to root B. Scan only root A: file marked
missing. Then scan root B: file appears as new (different UUID, because the
missing file's last path was under root A, not root B). Verifies cross-root
move isolation.

#### B. External modification detection

**B5. `scan_detects_external_modification`**
Scan a file. Overwrite its content on disk (append bytes to change hash).
Rescan. Verify: old UUID marked missing with path=NULL, External transition on
old UUID, new UUID created at same path, Discovery transition on new UUID.

**B6. `scan_external_mod_plus_deletion`**
Scan two files. Externally modify one, delete the other. Rescan. Verify both
operations handled correctly in a single reconciliation pass.

**B7. `scan_replace_with_identical_content`**
Scan a file. Delete it, then copy the same file back (same hash). Rescan.
Verify treated as unchanged (same UUID, no new transition).

#### C. Reactivation and lifecycle cycling

**C8. `scan_file_reappears_at_same_path`**
Scan file. Delete it. Rescan (marked missing). Restore the same file. Rescan.
Verify: same UUID, status back to active, missing_since cleared.

**C9. `scan_file_reappears_at_different_path`**
Scan file. Delete it. Rescan (marked missing). Copy back with different name.
Rescan. Verify: detected_move, same UUID, path updated.

**C10. `lifecycle_iteration_stress`**
Scaled by preset. Over N iterations: randomly delete, move, restore, and
externally modify files. After each iteration, scan and verify DB state
(correct active/missing counts, transition counts grow monotonically, no
duplicate UUIDs).

#### D. Process transition recording

**D11. `process_records_voom_transition`**
Process `hevc-surround` with a normalize policy. Verify `file_transitions` has
a row with `source='voom'`, `source_detail` containing the executor name and
phase name, `from_hash` and `to_hash` populated, and `plan_id` set.

**D12. `process_multi_phase_records_multiple_transitions`**
Write a policy with 2 phases (normalize + defaults, both producing changes on
`hevc-surround`). Process. Verify 2 Voom transitions for the same file, each
with a different `source_detail` phase name.

**D13. `process_dry_run_records_no_transitions`**
Process with `--dry-run`. Verify `file_transitions` has zero `source='voom'`
rows.

**D14. `process_updates_expected_hash`**
Process `hevc-surround`. Verify `files.expected_hash` is non-NULL and matches
the file's current on-disk hash. Rescan the same file. Verify it is treated as
unchanged (no External transition).

**D15. `process_then_rescan_shows_full_history`**
Scan, process, rescan. Run `history` command on the file. Verify output shows
discovery and voom transitions in chronological order.

#### E. Crash recovery

These tests simulate crashes by directly creating the state a crash would leave
behind: `.vbak` files and `event_log` entries.

**E16. `crash_recovery_always_recover`**
Setup: create `.voom-backup/<stem>.<timestamp>.vbak` file containing a copy of
the original. Insert a `plan.executing` event into `event_log` for that file
path (no `plan.completed` or `plan.failed`). Configure
`[recovery] mode = "always_recover"` in config.toml.
Run `process`. Verify: backup file deleted, original file restored to backup
content, transition recorded with `source='unknown'`,
`source_detail='crash_recovery:restored'`.

**E17. `crash_recovery_always_discard`**
Same setup as E16 but with `mode = "always_discard"`. Run `process`. Verify:
backup file deleted, on-disk file unchanged, transition recorded with
`source_detail='crash_recovery:discarded'`.

**E18. `normal_backup_not_treated_as_orphan`**
Create `.vbak` file. Insert both `plan.executing` AND `plan.completed` events
into `event_log`. Run `process`. Verify: backup file still exists (not
treated as orphan), no crash recovery transition recorded.

#### F. Statistics and filtering

**F19. `missing_files_excluded_from_status_and_report`**
Scan 2 files. Delete one. Rescan (1 missing). Verify `status` shows 1 file.
Verify `report` shows 1 file. Verify `report --format json` total is 1.

**F20. `db_prune_removes_soft_deleted_files`**
Scan file. Delete it. Rescan (marked missing). Manually set `missing_since` to
a date older than the retention window (via direct SQLite UPDATE). Run
`db prune`. Verify: file hard-deleted from `files` table, corresponding
`file_transitions` rows also deleted.

#### G. Corrupt file handling

**G21. `scan_handles_corrupt_files_gracefully`**
Generate corpus with `--corrupt N`. Scan the directory. Verify: scan completes
without panic/crash, valid files are tracked, corrupt files either produce
warnings or are skipped (depending on corruption type).

**G22. `process_handles_corrupt_files_gracefully`**
Scan a directory containing both valid and corrupt files. Process with a
policy. Verify: process completes, valid files processed normally, corrupt
files produce errors but don't abort the batch.

### Test execution

```bash
# Default (small) scale
cargo test -p voom-cli --features functional -- --test-threads=4

# Medium scale
VOOM_TEST_SCALE=medium cargo test -p voom-cli --features functional -- --test-threads=4

# Large scale
VOOM_TEST_SCALE=large cargo test -p voom-cli --features functional -- --test-threads=2
```

## Unchanged Components

- Existing `test_lifecycle` module (5 tests) — kept as-is
- All other existing functional test modules — unchanged
- `build_manifest()` — unchanged, still produces the same 11 specs
- `build_ffmpeg_cmd()` — unchanged, random specs use the same format

## Deferred

- Interactive crash recovery (`mode = "prompt"`) — not testable in non-interactive functional tests
- Purge retention window integration with `voom db maintenance` — requires time manipulation
