# Issue #202: HDR Tone Mapping and Metadata Preservation

## Goal

Add first-class HDR awareness to video transcodes:

- Detect HDR10, HDR10+, and Dolby Vision signals during ffprobe introspection.
- Preserve HDR10 static color metadata by default when transcoding HDR sources.
- Tone-map HDR sources to SDR when the policy requests SDR output.
- Surface optional HDR helper tools in environment checks.
- Document user-facing policy syntax, examples, and verification steps.

## Implementation Plan

1. Extend `Track` with optional color metadata: primaries, transfer, matrix,
   MaxCLL, MaxFALL, mastering display, and Dolby Vision profile.
2. Parse those values from ffprobe stream fields and `side_data_list`.
3. Persist the new track metadata in sqlite with migrations for existing
   databases.
4. Extend transcode settings with:
   - `preserve_hdr: true | false`
   - `tonemap: bt2390 | hable | mobius | reinhard | clip`
   - `hdr_color_metadata: copy`
   - `dolby_vision: copy_rpu`
5. Keep `hdr_mode: preserve | tonemap` as the compact existing form.
6. Generate ffmpeg HDR10 preservation arguments from detected track metadata.
7. Generate SDR BT.709 tone-map filters and output color metadata when
   `hdr_mode: tonemap`, `preserve_hdr: false`, or `tonemap` is present.
8. Add `hdr10plus_tool` and `dovi_tool` to tool detection and environment
   reporting.

## Functional Test Plan

Generate a synthetic corpus:

```sh
scripts/generate-test-corpus /tmp/voom-hdr-corpus --count 12 --seed 202
```

The built-in manifest includes `4k-hevc-hdr10.mkv`, generated with BT.2020,
SMPTE ST 2084, HEVC Main10 pixel format, mastering display metadata, and
MaxCLL/MaxFALL.

HDR10 preservation:

```sh
voom process /tmp/voom-hdr-corpus \
  --policy docs/examples/hdr-archival.voom \
  --dry-run
```

Then run without `--dry-run` on a disposable copy and verify the output:

```sh
ffprobe -v error -select_streams v:0 \
  -show_entries stream=color_primaries,color_transfer,color_space,pix_fmt \
  -show_entries stream_side_data \
  -of json /tmp/voom-hdr-output/4k-hevc-hdr10.mkv
```

Expected: `color_primaries=bt2020`, `color_transfer=smpte2084`,
`color_space=bt2020nc`, a 10-bit pixel format, and static HDR side data.

SDR tone mapping:

```sh
voom process /tmp/voom-hdr-corpus \
  --policy docs/examples/hdr-sdr-mobile.voom
```

Verify output transfer characteristics:

```sh
ffprobe -v error -select_streams v:0 \
  -show_entries stream=color_primaries,color_transfer,color_space,pix_fmt \
  -of default=nw=1 /tmp/voom-hdr-output/4k-hevc-hdr10.mkv
```

Expected: `color_primaries=bt709`, `color_transfer=bt709`,
`color_space=bt709`, and `pix_fmt=yuv420p`.

Environment checks:

```sh
voom env check
voom tools list --format json
```

Expected: `hdr10plus_tool` and `dovi_tool` are listed as optional tools when
present and reported as not found when absent.

## Adversarial Review Checklist

- Verify HDR preservation is opt-out only for detected HDR sources, so SDR
  transcodes do not receive HDR flags.
- Verify explicit tone mapping still emits SDR filters even if test fixtures
  omit HDR metadata.
- Verify unsupported HDR preservation targets fail with actionable messages.
- Verify sqlite migrations preserve old databases and old JSON records.
- Verify examples parse and do not document unimplemented policy syntax.
- Verify functional tests inspect the output media, not only VOOM plan text.

## Known Follow-up

HDR10+ dynamic metadata and Dolby Vision RPU extraction/injection require
multi-step executor workflows around `hdr10plus_tool` and `dovi_tool`. This
change surfaces tool availability and preserves static HDR10 metadata, but the
dynamic metadata reinjection path should be implemented as a separate executor
slice so it can be reviewed with real sample fixtures.
