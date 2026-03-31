# Functional Test Plan: english-optimized Policies

Manual test plan for `english-optimized.voom` and `english-optimized-background.voom`.

## Prerequisites

- `ffmpeg`, `ffprobe`, `mkvmerge`, `mkvpropedit` on PATH
- Test corpus generated: `python3 scripts/generate-test-corpus /tmp/voom-corpus --duration 2 --seed 42`
- Working voom install: `cargo build && voom doctor`

## 1. Policy Validation

Confirm both policies parse, compile, and format without errors or warnings.

```bash
voom policy validate docs/examples/english-optimized.voom
voom policy validate docs/examples/english-optimized-background.voom
voom policy show docs/examples/english-optimized.voom
voom policy show docs/examples/english-optimized-background.voom
```

**Expected:** All commands succeed with `OK`. `policy show` displays 9 phases each with correct dependency chains.

Compare the two policies to verify the background variant only differs in `on_error` settings:

```bash
voom policy diff docs/examples/english-optimized.voom \
                 docs/examples/english-optimized-background.voom
```

**Expected:** Differences limited to per-phase `on_error: continue` in the background variant and the policy name.

## 2. Dry-Run Smoke Test

Run both policies against the test corpus without modifying files.

```bash
# Copy corpus to a scratch directory (dry-run shouldn't modify, but be safe)
cp -r /tmp/voom-corpus /tmp/voom-test-eo
voom process /tmp/voom-test-eo --policy docs/examples/english-optimized.voom --dry-run
```

**Expected:**
- Exits successfully
- Every corpus file gets a plan summary
- No files modified (verify with `shasum /tmp/voom-test-eo/*` before and after)

## 3. Plan Inspection

Use `--plan-only` to get machine-readable plans for detailed review.

```bash
voom process /tmp/voom-test-eo \
  --policy docs/examples/english-optimized.voom \
  --plan-only > /tmp/plans-eo.json
```

Inspect the JSON and verify each test scenario below.

## 4. Phase-by-Phase Scenarios

### 4.1 Containerize (phase 1)

| Test file | Expected behavior |
|-----------|-------------------|
| `basic-h264-aac.mp4` | Plan includes `ConvertContainer` to MKV |
| `sd-mpeg2.avi` | Plan includes `ConvertContainer` to MKV |
| `vp9-opus.webm` | Plan includes `ConvertContainer` to MKV |
| `hevc-surround.mkv` | No container action (already MKV) |

**Verify:** After live run, all output files have `.mkv` extension.

### 4.2 Strip (phase 2)

| Test file | Expected behavior |
|-----------|-------------------|
| `attachment.mkv` | `ClearTags` + `RemoveAttachments` (non-font attachments removed, font kept) |
| `cover-art.mkv` | Cover art attachment removed (not a font) |
| `hevc-surround.mkv` | `ClearTags` emitted |

**Verify:** `voom inspect <file> --format json` shows no container tags and no non-font attachments after processing.

### 4.3 Transcode (phase 3)

| Test file | Expected behavior |
|-----------|-------------------|
| `basic-h264-aac.mp4` | Plan includes `TranscodeVideo` to HEVC (h264 source) |
| `sd-mpeg2.avi` | Plan includes `TranscodeVideo` to HEVC (mpeg2 source) |
| `hevc-surround.mkv` | **Skipped** via `skip when video.codec == "hevc"` |
| `4k-hevc-hdr10.mkv` | **Skipped** (already HEVC) |

**Verify:** After live run, `voom inspect <file>` shows `hevc` video codec on previously non-HEVC files. HDR10 metadata preserved on `4k-hevc-hdr10.mkv`.

### 4.4 Enrich (phase 4)

This phase requires Radarr/Sonarr plugin metadata. Without those plugins configured, it should be a no-op.

**Test without metadata:** Run `--plan-only` and confirm no `SetLanguage` actions in the enrich phase.

**Test with simulated metadata:** If you have a Radarr/Sonarr integration configured, or can inject `plugin_metadata` via a WASM plugin, verify:
- Video track language set to `original_language` from Radarr
- First matching rule wins (`rules first` mode): if Radarr metadata exists, Sonarr rule is skipped

### 4.5 Audio Cleanup (phase 5a)

| Test file | Expected behavior |
|-----------|-------------------|
| `multichannel-flac.mkv` | Commentary audio track (ac3 with "Director's Commentary" title) removed |
| Any file with `zxx` audio | Track removed |

**Verify:** `voom inspect` after processing shows no commentary or `zxx` audio tracks.

### 4.6 Audio Filtering (phases 5b/5c)

These phases are mutually exclusive via `skip when` conditions:

| Scenario | Active phase | Behavior |
|----------|-------------|----------|
| No Radarr metadata | `audio-filter-english` | Keep only `eng`/`und` audio |
| Radarr `original_language == "eng"` | `audio-filter-english` | Keep only `eng`/`und` audio |
| Radarr `original_language == "jpn"` | `audio-filter-foreign` | Keep `eng` + `jpn` audio |

**Verify with `--plan-only`:** Confirm exactly one of the two filter phases produces actions per file. The other should be skipped.

### 4.7 Subtitle Filtering (phase 6)

| Test file | Expected behavior |
|-----------|-------------------|
| `hevc-surround.mkv` | English subtitle kept, Spanish subtitle removed |
| `multichannel-flac.mkv` | English subtitle kept |
| `4k-hevc-hdr10.mkv` | English ASS subtitle kept |

**Verify:** Only `eng` non-commentary subtitles remain after processing.

### 4.8 Audio Normalization (phase 7)

Three synthesize rules, tested in priority order:

| Source audio | Expected synthesize |
|-------------|-------------------|
| eng TrueHD/DTS-HD/FLAC 7.1+ | EAC3 5.1 @ 640k created (unless AC3/EAC3 5.1+ already exists) |
| eng non-AC3/EAC3 5.1 | EAC3 5.1 @ 640k created (unless AC3/EAC3 5.1+ already exists) |
| eng stereo only (no 5.1+) | AAC stereo @ 192k created (unless AAC stereo already exists) |
| eng AC3 5.1 already present | No synthesize (skip_if_exists triggers) |

**Key corpus files:**
- `hevc-surround.mkv` (AC3 5.1 + AAC stereo): skip_if_exists should prevent new synthesis
- `multichannel-flac.mkv` (FLAC stereo): AAC stereo synthesize may trigger depending on track language

**Verify with `--plan-only`:** Check `SynthesizeAudio` actions and their `skip_if_exists` / `create_if` evaluation.

### 4.9 Finalize (phase 8)

**Verify:** Track order after processing matches: video, main audio, alternate audio, main subtitle, forced subtitle, attachments. Default audio is `first_per_language`, default subtitle is `none`.

Use `voom inspect <file> --format json` and check track ordering and default flags.

### 4.10 Validate (phase 9)

| Scenario | Expected behavior |
|----------|-------------------|
| File with eng audio remaining | No warnings or failures |
| File where all audio was removed | `fail` message: "No non-commentary audio tracks remain" |
| File with no eng audio remaining | `warn` message: "No English audio" |

**Test the failure case:** Create a policy variant that removes all audio, then run `--dry-run` and confirm the validation phase emits the fail message.

## 5. Background Variant Differences

Run the background policy and verify these behavioral differences:

```bash
voom process /tmp/voom-test-eo \
  --policy docs/examples/english-optimized-background.voom \
  --dry-run
```

**Verify:**
- `on_error: continue` on every phase: if containerize fails on a file, subsequent phases still attempt to run
- `on_error: abort` is NOT present (unlike the base policy's containerize phase)
- Processing completes even if individual files fail

## 6. Live Execution (Destructive)

Run against a disposable copy of the corpus with backups enabled.

```bash
cp -r /tmp/voom-corpus /tmp/voom-live-test

# Scan first to populate the database
voom scan /tmp/voom-live-test

# Process with backups
voom process /tmp/voom-live-test \
  --policy docs/examples/english-optimized.voom

# Verify backups were created
voom backup list /tmp/voom-live-test
```

**Verify after processing:**
- All files are MKV containers
- Video tracks are HEVC (except files that were already HEVC)
- Only English non-commentary subtitles remain
- Commentary audio tracks removed
- Backup files (`.vbak`) exist for every modified file
- `voom inspect <file>` shows expected track layout

**Re-run idempotency check:**

```bash
voom process /tmp/voom-live-test \
  --policy docs/examples/english-optimized.voom \
  --dry-run
```

**Expected:** All plans should be empty or skipped on the second run (policy is already satisfied).

## 7. Edge Cases

| Scenario | How to test | Expected |
|----------|------------|----------|
| Empty directory | `voom process /tmp/empty --policy ...` | "No media files found", exit 0 |
| Single file | `voom process /tmp/voom-live-test/hevc-surround.mkv --policy ...` | Processes just that file |
| File with no audio | Remove all audio from a test file with mkvmerge, then process | Validate phase emits fail |
| Already-optimized file | Process an already-processed file | All phases skip or produce empty plans |
| Corrupt/truncated file | Truncate a corpus file: `head -c 1024 file.mkv > bad.mkv` | Error reported, processing continues (background policy) or aborts (base policy) |

## 8. Automated Functional Tests

The existing functional test suite can run these policies:

```bash
cargo test -p voom-cli --features functional -- --test-threads=4
```

To add policy-specific automated tests, add cases to `crates/voom-cli/tests/functional_tests.rs` following the `test_process` module pattern, using `env.write_policy()` with the policy content and `env.populate_media()` with appropriate corpus stems.
