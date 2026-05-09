# HDR Transcoding

VOOM detects HDR video metadata during ffprobe introspection and carries that
metadata into video transcode actions.

## Preserve HDR

HDR preservation is the default for detected HDR sources:

```voom
policy "hdr-archive" {
  phase transcode {
    transcode video to hevc {
      crf: 18
      preset: slow
      preserve_hdr: true
      hdr_color_metadata: copy
    }
  }
}
```

For HDR10 sources, VOOM copies BT.2020/SMPTE ST 2084 color metadata,
MaxCLL/MaxFALL, and mastering-display values into the ffmpeg command. HEVC
software transcodes use `-x265-params`; hardware encoders receive ffmpeg color
metadata flags and a 10-bit pixel format.

## Tone Map To SDR

Set `preserve_hdr: false`, `hdr_mode: tonemap`, or `tonemap: <algorithm>` to
produce SDR output:

```voom
policy "sdr-mobile" {
  phase mobile {
    transcode video to hevc {
      max_resolution: 1080p
      crf: 24
      preserve_hdr: false
      tonemap: bt2390
    }
  }
}
```

Supported tone-map algorithms are `bt2390`, `hable`, `mobius`, `reinhard`,
and `clip`. SDR outputs are tagged as BT.709 and use `yuv420p`.

## Tooling

Run:

```sh
voom env check
voom tools list
```

`hdr10plus_tool` and `dovi_tool` are optional. They are required for future
dynamic HDR10+ metadata and Dolby Vision RPU reinjection workflows; VOOM reports
their availability so policy authors can see the current environment limits.

## Test Corpus

Use the synthetic corpus generator for disposable HDR test content:

```sh
scripts/generate-test-corpus /tmp/voom-hdr-corpus --count 12 --seed 202
```

The generated `4k-hevc-hdr10.mkv` sample includes HDR10 static metadata and is
safe for preservation and tone-map regression tests.
