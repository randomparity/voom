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

### Setup

Create a fresh test copy for the live run:

```bash
rm -rf /tmp/voom-test-eo
cp -r /tmp/voom-corpus /tmp/voom-test-eo
```

Run the live process:

```bash
voom process /tmp/voom-test-eo --policy docs/examples/english-optimized.voom
```

**Expected:** `0 errors` in the summary line, all phases report completed or skipped counts.

### 4.1 Containerize (phase 1)

| Test file | Expected behavior |
|-----------|-------------------|
| `basic-h264-aac.mp4` | Plan includes `ConvertContainer` to MKV |
| `sd-mpeg2.avi` | Plan includes `ConvertContainer` to MKV |
| `vp9-opus.webm` | Plan includes `ConvertContainer` to MKV |
| `av1-opus.mp4` | Plan includes `ConvertContainer` to MKV |
| `vfr-h264.mp4` | Plan includes `ConvertContainer` to MKV |
| `hevc-surround.mkv` | No container action (already MKV) |

**Verify all files are now MKV:**

```bash
# Should list only .mkv files (plus any .vbak backups)
ls /tmp/voom-test-eo/*.mkv
# Should return nothing — no non-MKV media files remain
ls /tmp/voom-test-eo/*.{mp4,avi,webm} 2>/dev/null
```

**Verify in plan-only output:**

```bash
jq '[.[] | select(.phase == "containerize")] |
    group_by(.file.path | split("/")[-1]) |
    .[] | {file: .[0].file.path | split("/")[-1],
           actions: [.[].actions[].operation]}' /tmp/plans-eo.json
```

### 4.2 Strip (phase 2)

| Test file | Expected behavior |
|-----------|-------------------|
| `attachment.mkv` | `ClearTags` + `RemoveAttachments` (non-font attachments removed, font kept) |
| `cover-art.mkv` | Cover art attachment removed (not a font) |
| All MKV files | `ClearTags` emitted |

**Verify no container-level tags remain:**

```bash
# Should show no tags on any file
for f in /tmp/voom-test-eo/*.mkv; do
  tags=$(ffprobe -v quiet -print_format json -show_format "$f" | jq '.format.tags // {}')
  echo "$(basename "$f"): $tags"
done
```

**Verify attachment.mkv kept font but removed images:**

```bash
# Should show only the font attachment (DummyFont.ttf), not cover.jpg or poster.png
voom inspect /tmp/voom-test-eo/attachment.mkv --format json | \
  jq '.tracks[] | select(.track_type | test("attachment"; "i")) | {index, title, codec}'
```

**Verify cover-art.mkv has no attachments:**

```bash
voom inspect /tmp/voom-test-eo/cover-art.mkv --format json | \
  jq '[.tracks[] | select(.track_type | test("attachment"; "i"))] | length'
# Expected: 0
```

### 4.3 Transcode (phase 3)

| Test file | Expected behavior |
|-----------|-------------------|
| `basic-h264-aac.mkv` | Transcoded to HEVC (was h264) |
| `sd-mpeg2.mkv` | Transcoded to HEVC (was mpeg2) |
| `vp9-opus.mkv` | Transcoded to HEVC (was vp9) |
| `vfr-h264.mkv` | Transcoded to HEVC (was h264) |
| `multichannel-flac.mkv` | Transcoded to HEVC (was h264) |
| `hevc-surround.mkv` | **Skipped** via `skip when video.codec == "hevc"` |
| `4k-hevc-hdr10.mkv` | **Skipped** (already HEVC) |
| `hevc-truehd.mkv` | **Skipped** (already HEVC) |

**Verify all video tracks are now HEVC:**

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  codec=$(ffprobe -v quiet -print_format json -show_streams -select_streams v "$f" | \
    jq -r '.streams[0].codec_name')
  echo "$(basename "$f"): $codec"
done
# Expected: every file shows "hevc"
```

**Verify skip_when in plan-only output:**

```bash
jq '.[] | select(.phase == "transcode" and .skip_reason != null) |
    {file: .file.path | split("/")[-1], skip_reason}' /tmp/plans-eo.json
```

**Verify HDR10 metadata preserved on 4k-hevc-hdr10.mkv:**

```bash
ffprobe -v quiet -print_format json -show_streams -select_streams v \
  /tmp/voom-test-eo/4k-hevc-hdr10.mkv | \
  jq '{color_primaries: .streams[0].color_primaries,
       color_transfer: .streams[0].color_transfer,
       color_space: .streams[0].color_space}'
# Expected: bt2020 / smpte2084 / bt2020nc
```

### 4.4 Enrich (phase 4)

This phase requires Radarr/Sonarr plugin metadata. Without those plugins configured, it should be a no-op.

**Verify no SetLanguage actions without metadata:**

```bash
jq '[.[] | select(.phase == "enrich") | .actions[]] | length' /tmp/plans-eo.json
# Expected: 0
```

**Test with simulated metadata:** If you have a Radarr/Sonarr integration configured, or can inject `plugin_metadata` via a WASM plugin, verify:
- Video track language set to `original_language` from Radarr
- First matching rule wins (`rules first` mode): if Radarr metadata exists, Sonarr rule is skipped

### 4.5 Audio Cleanup (phase 5a)

| Test file | Expected behavior |
|-----------|-------------------|
| `multichannel-flac.mkv` | Commentary audio track (ac3 with "Director's Commentary" title) removed |
| Any file with `zxx` audio | Track removed |

**Verify commentary track removed from multichannel-flac.mkv:**

```bash
voom inspect /tmp/voom-test-eo/multichannel-flac.mkv --format json | \
  jq '[.tracks[] | select(.track_type | test("audio"; "i"))] |
      [{index, codec, title, language}]'
# Expected: only the FLAC stereo track remains, no "Director's Commentary"
```

**Verify in plan-only output:**

```bash
jq '.[] | select(.phase == "audio-cleanup" and
    (.file.path | contains("multichannel"))) |
    {file: .file.path | split("/")[-1],
     actions: [.actions[] | {operation, description}]}' /tmp/plans-eo.json
```

### 4.6 Audio Filtering (phases 5b/5c)

These phases are mutually exclusive via `skip when` conditions:

| Scenario | Active phase | Behavior |
|----------|-------------|----------|
| No Radarr metadata | `audio-filter-english` | Keep only `eng`/`und` audio |
| Radarr `original_language == "eng"` | `audio-filter-english` | Keep only `eng`/`und` audio |
| Radarr `original_language == "jpn"` | `audio-filter-foreign` | Keep `eng` + `jpn` audio |

**Verify mutual exclusivity — exactly one phase skipped per file:**

```bash
jq '[.[] | select(.phase | test("audio-filter"))] |
    group_by(.file.path) | .[] |
    {file: .[0].file.path | split("/")[-1],
     phases: [.[] | {phase: .phase, skipped: (.skip_reason != null)}]}' \
  /tmp/plans-eo.json
# Expected: for each file, audio-filter-foreign is skipped (no Radarr metadata),
#           audio-filter-english is active
```

**Verify remaining audio tracks have eng or und language:**

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  langs=$(voom inspect "$f" --format json | \
    jq -r '[.tracks[] | select(.track_type | test("audio"; "i")) | .language] | join(",")')
  echo "$(basename "$f"): $langs"
done
# Expected: only "eng" and/or "und" languages
```

### 4.7 Subtitle Filtering (phase 6)

| Test file | Expected behavior |
|-----------|-------------------|
| `hevc-surround.mkv` | English subtitle kept, Spanish subtitle removed |
| `multichannel-flac.mkv` | English subtitle kept |
| `4k-hevc-hdr10.mkv` | English ASS subtitle kept |

**Verify only English non-commentary subtitles remain:**

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  subs=$(voom inspect "$f" --format json | \
    jq '[.tracks[] | select(.track_type | test("subtitle"; "i")) |
         {language, title}]')
  count=$(echo "$subs" | jq 'length')
  if [ "$count" -gt 0 ]; then
    echo "$(basename "$f"): $subs"
  fi
done
# Expected: only eng-language subtitles, no commentary subtitles
```

**Verify Spanish subtitle removed from hevc-surround.mkv:**

```bash
voom inspect /tmp/voom-test-eo/hevc-surround.mkv --format json | \
  jq '[.tracks[] | select(.track_type | test("subtitle"; "i")) |
       {language, title}]'
# Expected: only English subtitle, no Spanish
```

### 4.8 Audio Normalization (phase 7)

Three synthesize rules, tested in priority order:

| Source audio | Expected synthesize |
|-------------|-------------------|
| eng TrueHD/DTS-HD/FLAC 7.1+ | EAC3 5.1 @ 640k created (unless AC3/EAC3 5.1+ already exists) |
| eng non-AC3/EAC3 5.1 | EAC3 5.1 @ 640k created (unless AC3/EAC3 5.1+ already exists) |
| eng stereo only (no 5.1+) | AAC stereo @ 192k created (unless AAC stereo already exists) |
| eng AC3 5.1 already present | No synthesize (skip_if_exists triggers) |

**Verify synthesized tracks in plan-only output:**

```bash
jq '.[] | select(.phase == "audio-normalize" and .actions != null and
    (.actions | length) > 0) |
    {file: .file.path | split("/")[-1],
     actions: [.actions[] | {operation, description}]}' /tmp/plans-eo.json
```

**Verify hevc-surround.mkv skip_if_exists (already has AC3 5.1):**

```bash
jq '.[] | select(.phase == "audio-normalize" and
    (.file.path | contains("hevc-surround"))) |
    {actions_count: (.actions | length),
     actions: [.actions[]? | .description]}' /tmp/plans-eo.json
# Expected: no SynthesizeAudio actions (AC3 5.1 already satisfies skip_if_exists)
```

**Verify audio tracks after processing:**

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  tracks=$(voom inspect "$f" --format json | \
    jq '[.tracks[] | select(.track_type | test("audio"; "i")) |
         {codec, channels, language, title}]')
  echo "=== $(basename "$f") ==="
  echo "$tracks"
done
```

### 4.9 Finalize (phase 8)

**Verify track order matches policy spec:**

Track order should be: video, main audio, alternate audio, main subtitle, forced subtitle, attachments.

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  order=$(voom inspect "$f" --format json | \
    jq '[.tracks[] | {index, track_type, codec, language}]')
  echo "=== $(basename "$f") ==="
  echo "$order"
done
```

**Verify default flags — audio: first_per_language, subtitle: none:**

```bash
for f in /tmp/voom-test-eo/*.mkv; do
  defaults=$(voom inspect "$f" --format json | \
    jq '{audio_defaults: [.tracks[] | select(.track_type | test("audio"; "i")) |
             {index, language, default: .default}],
         sub_defaults: [.tracks[] | select(.track_type | test("subtitle"; "i")) |
             {index, language, default: .default}]}')
  echo "=== $(basename "$f") ==="
  echo "$defaults"
done
# Expected: first audio per language has default=true; all subtitles have default=false
```

### 4.10 Validate (phase 9)

| Scenario | Expected behavior |
|----------|-------------------|
| File with eng audio remaining | No warnings or failures |
| File where all audio was removed | `fail` message: "No non-commentary audio tracks remain" |
| File with no eng audio remaining | `warn` message: "No English audio" |

**Check validate phase in plan-only output:**

```bash
jq '.[] | select(.phase == "validate") |
    {file: .file.path | split("/")[-1],
     actions: (.actions | length),
     warnings: .warnings,
     skip_reason}' /tmp/plans-eo.json
# Expected: validate runs (not skipped) and produces no fail/warn actions
#           for files with English audio
```

**Test the failure case:** Create a policy variant that removes all audio, then confirm the validation phase catches it:

```bash
cat > /tmp/test-validate.voom <<'POLICY'
policy "validate-test" {
  phase strip-all-audio {
    remove audio where true
  }
  phase validate {
    depends_on: [strip-all-audio]
    run_if strip-all-audio.completed
    when count(audio where not commentary) == 0 {
      fail "No non-commentary audio tracks remain in {filename}"
    }
  }
}
POLICY

voom process /tmp/voom-test-eo --policy /tmp/test-validate.voom --plan-only 2>/dev/null | \
  jq '.[] | select(.phase == "validate" and (.actions | length) > 0) |
      {file: .file.path | split("/")[-1],
       actions: [.actions[] | .description]}' | head -20
# Expected: fail actions for every file
```

## 5. Background Variant Differences

Run the background policy and verify these behavioral differences:

```bash
rm -rf /tmp/voom-test-eo
cp -r /tmp/voom-corpus /tmp/voom-test-eo
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
rm -rf /tmp/voom-live-test
cp -r /tmp/voom-corpus /tmp/voom-live-test

# Scan first to populate the database
voom scan /tmp/voom-live-test

# Process with backups
voom process /tmp/voom-live-test \
  --policy docs/examples/english-optimized.voom
```

**Verify after processing:**

```bash
# All files are MKV containers
ls /tmp/voom-live-test/*.mkv

# No non-MKV media files remain
ls /tmp/voom-live-test/*.{mp4,avi,webm} 2>/dev/null

# Video tracks are HEVC
for f in /tmp/voom-live-test/*.mkv; do
  codec=$(ffprobe -v quiet -print_format json -show_streams -select_streams v "$f" | \
    jq -r '.streams[0].codec_name')
  echo "$(basename "$f"): $codec"
done

# Only English non-commentary subtitles remain
for f in /tmp/voom-live-test/*.mkv; do
  voom inspect "$f" --format json | \
    jq -e '[.tracks[] | select(.track_type | test("subtitle"; "i")) |
            select(.language != "eng" or (.title // "" | test("commentary"; "i")))] |
           length == 0' > /dev/null && echo "$(basename "$f"): OK" || echo "$(basename "$f"): FAIL"
done

# Backup files exist for every modified file
ls /tmp/voom-live-test/*.vbak | wc -l
```

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

**Edge case commands:**

```bash
# Empty directory
mkdir -p /tmp/empty
voom process /tmp/empty --policy docs/examples/english-optimized.voom

# Single file
cp /tmp/voom-corpus/hevc-surround.mkv /tmp/voom-single/
voom process /tmp/voom-single --policy docs/examples/english-optimized.voom

# File with no audio
mkdir -p /tmp/voom-no-audio
mkvmerge -o /tmp/voom-no-audio/no-audio.mkv --no-audio /tmp/voom-live-test/hevc-surround.mkv
voom process /tmp/voom-no-audio --policy docs/examples/english-optimized.voom --dry-run

# Already-optimized file (re-run on processed output)
voom process /tmp/voom-live-test --policy docs/examples/english-optimized.voom --dry-run
# Expected: 0 would modify

# Corrupt file
mkdir -p /tmp/voom-corrupt
head -c 1024 /tmp/voom-corpus/hevc-surround.mkv > /tmp/voom-corrupt/bad.mkv
voom process /tmp/voom-corrupt --policy docs/examples/english-optimized-background.voom
# Expected: error reported, exit 0 (background policy continues)
```

## 8. Automated Functional Tests

The existing functional test suite can run these policies:

```bash
cargo test -p voom-cli --features functional -- --test-threads=4
```

To add policy-specific automated tests, add cases to `crates/voom-cli/tests/functional_tests.rs` following the `test_process` module pattern, using `env.write_policy()` with the policy content and `env.populate_media()` with appropriate corpus stems.
