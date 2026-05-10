# Test Corpus Generator Coverage Design

## Purpose

Improve `scripts/generate-test-corpus` so it produces deterministic media
fixtures that cover the supported VOOM feature surface while preserving fast
CI-friendly generation. The generator should also continue to support optional
random files and random corruption for fuzz-style local testing.

The corpus must include procedural motion content, not only static images or
test bars. The default procedural source should be high-detail generated video,
such as Mandelbrot/fractal zoom content, tuned by resolution so quality loss is
visible after later transcoding and frame preview workflows.

## Scope

This design only changes the corpus generation script and its generated
manifest. It does not change VOOM processing, introspection, transcoding,
reporting, or GUI behavior.

## Generator Shape

Keep `scripts/generate-test-corpus` as the single entry point. Refactor the
deterministic fixture manifest so each fixture declares explicit metadata:

- `stem` and `ext`
- `profiles`
- `covers`
- `expect`
- `video`, `audio`, `subs`, and `special` generation settings
- optional encoder requirements
- optional fixture-specific duration

Example fixture shape:

```python
{
    "stem": "mandelbrot-hdr10-4k",
    "ext": "mkv",
    "profiles": ["coverage", "stress"],
    "covers": [
        "video.content.mandelbrot_zoom",
        "video.resolution.4k",
        "video.hdr.hdr10",
        "video.codec.hevc",
    ],
    "expect": {
        "bad_file": False,
        "video_codec": "hevc",
        "is_hdr": True,
        "hdr_format": "hdr10",
        "width": 3840,
        "height": 2160,
    },
    "video": {...},
    "audio": [...],
    "subs": [...],
    "special": [...],
}
```

Add `--profile smoke|coverage|stress|all`.

- `smoke`: a minimal fast set for CI, roughly five to eight short files.
- `coverage`: all named product-feature fixtures with short durations.
- `stress`: slow or heavy fixtures such as 4K HDR, many-track files, and larger
  variants of corruption cases.
- `all`: coverage plus stress.

Default to `coverage` for local runs. Existing `--only`, `--skip`, `--count`,
`--duration`, `--duration-range`, and `--corrupt` behavior should continue to
work. `--only` and `--skip` apply after profile selection.

## Manifest Output

Write a `manifest.json` sidecar into the destination directory for non-dry-run
generation. The manifest records:

- manifest `schema_version`
- selected profile and command-relevant settings
- generated files
- skipped files and skip reasons
- failed files and failure summaries
- coverage tags per fixture
- expected traits per fixture
- deterministic and random corruption details

Integration tests should consume this manifest instead of duplicating fixture
knowledge in test code.

## Fixture Families

### Video Content

Use procedural FFmpeg sources. Mandelbrot/fractal zoom content is the primary
source for meaningful motion and fine detail. Keep `testsrc2` only for cheap
smoke fixtures where content quality is not the point.

Resolution tiers should be tuned so later transcoding visibly loses quality:

- SD fixtures should expose blocking and ringing.
- 1080p fixtures should include high-frequency detail and smooth motion.
- 4K fixtures should stress codec behavior with dense texture and motion.
- VFR fixtures should use procedural motion plus timestamp variation.

### HDR And Color

Add named fixtures for:

- HDR10 HEVC, 10-bit, BT.2020, PQ transfer.
- HLG HEVC, 10-bit, BT.2020, ARIB STD-B67 transfer.
- SDR 10-bit HEVC that must not be classified as HDR.
- Incomplete HDR-like metadata only if FFmpeg and ffprobe expose enough fields
  to assert conservative detection clearly.

Avoid fake Dolby Vision fixtures unless VOOM can detect real Dolby Vision
metadata from generated files.

### Black Bars And Crop Detection

Add named crop fixtures:

- Letterbox.
- Pillarbox.
- Windowbox.
- No-bars content with dark edges, to catch false positives.
- Intermittent black frames or black transitions if crop sampling needs that
  edge case.

### Audio Normalization

Add named fixtures for:

- Quiet dialogue-like mix.
- Hot compressed mix.
- Already normalized target generated with FFmpeg `loudnorm`.
- Dynamic range and silence-plus-bursts.
- 5.1 and 7.1 preservation.
- Commentary and alternate-language tracks.
- AAC, AC3, EAC3, FLAC, and Opus where the local FFmpeg build supports them.

Relative loudness fixtures test behavior coverage. The already-normalized
fixture gives reporting and skip behavior a stable measurable target.

### Subtitles And Attachments

Keep and expand existing coverage:

- SRT, ASS, and WebVTT subtitles.
- Forced, default, and commentary subtitle metadata.
- Font attachment.
- Cover and poster image attachments.
- Multi-language audio and subtitle combinations.

### Corruption

Add deterministic named corrupt fixtures by generating a valid source file and
then applying a named corruption transform into a separate output file.

Corruption cases:

- Truncated tail.
- Zero-length file.
- Header damage.
- Midstream bit rot.
- Wrong extension.
- Corrupt metadata or container header where feasible.
- Valid container with damaged media stream where feasible.

Keep `--corrupt N/%` as optional random post-processing fuzz against random
generated files. Random corruption must not replace deterministic corrupt
fixtures.

## Behavior And Boundaries

`--duration` applies to deterministic fixtures unless a fixture declares its own
duration. Heavy fixtures may cap or override duration to keep runtime
controlled. Random fixtures continue to use `--duration-range`.

Encoder-dependent fixtures skip cleanly when the local FFmpeg build lacks the
required encoder. The skip reason is recorded in `manifest.json`.

Validation inside the generator stays lightweight:

- Verify FFmpeg exists.
- Probe available encoders.
- Generate files.
- Apply deterministic and optional random corruptions.
- Write the manifest.

The generator should not run VOOM or deeply assert ffprobe output. Integration
tests can use `manifest.json` for that.

## Testing And Rollout

Implement in small steps:

1. Add fixture metadata, profile selection, and `manifest.json` output while
   preserving current fixtures.
2. Add procedural Mandelbrot/video-source support and move selected fixtures to
   it.
3. Add deterministic corrupt fixtures.
4. Expand HDR, crop, and audio fixture coverage.
5. Add focused tests or dry-run checks for profile selection, manifest output,
   corruption reporting, and encoder skip reporting.

Verification for the script should use cheap checks:

- `scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile smoke`
- `scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile coverage`
- `scripts/generate-test-corpus /tmp/voom-corpus-smoke --profile smoke --duration 1`
- JSON validation of the generated `manifest.json`

Run broader VOOM or media-processing tests only when they directly consume the
generated corpus behavior.
