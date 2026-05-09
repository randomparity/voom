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

## Preserve HDR10+ and Dolby Vision

HDR10+ and Dolby Vision carry dynamic metadata outside the static HDR10 color
fields. When an HDR10+ or Dolby Vision source is transcoded to HEVC with HDR
preservation enabled, VOOM extracts that metadata before the encode, injects it
into the encoded HEVC stream, and remuxes the injected stream into the output
container.

HDR10+ preservation requires `hdr10plus_tool` on `PATH`:

```voom
policy "hdr10plus-preserve" {
  phase preserve-dynamic-hdr {
    transcode video to hevc {
      crf: 18
      preset: slow
      preserve_hdr: true
      hdr_color_metadata: copy
    }
  }
}
```

Dolby Vision RPU preservation requires `dovi_tool` on `PATH` and supports
profiles 5, 7, and 8:

```voom
policy "dolby-vision-rpu" {
  phase preserve-rpu {
    transcode video to hevc {
      crf: 18
      preset: slow
      preserve_hdr: true
      hdr_color_metadata: copy
      dolby_vision: copy_rpu
    }
  }
}
```

Dynamic metadata preservation currently supports HEVC output in MKV or MP4.
VOOM fails before encoding when a required tool is missing, when Dolby Vision
profile metadata is missing or unsupported, or when the target codec/container
cannot carry the reinjected HEVC stream. Set `preserve_hdr: false`,
`hdr_mode: tonemap`, or `tonemap: <algorithm>` when you want SDR output instead
of dynamic HDR preservation.

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

`hdr10plus_tool` and `dovi_tool` are optional unless a policy preserves dynamic
HDR metadata from an HDR10+ or Dolby Vision source. VOOM reports their
availability so policy authors can see the current environment limits.

After a run, verify dynamic metadata with the HDR tools rather than only the
VOOM plan text:

```sh
ffmpeg -hide_banner -i output.mkv -map 0:v:0 -c copy \
  -bsf:v hevc_mp4toannexb -f hevc /tmp/output.hevc
hdr10plus_tool extract /tmp/output.hevc -o /tmp/output-hdr10plus.json
dovi_tool extract-rpu /tmp/output.hevc -o /tmp/output-rpu.bin
dovi_tool info -i /tmp/output-rpu.bin --summary
```

## Test Corpus

Use the synthetic corpus generator for disposable HDR test content:

```sh
scripts/generate-test-corpus /tmp/voom-hdr-corpus --count 12 --seed 202
```

The generated `4k-hevc-hdr10.mkv` sample includes HDR10 static metadata and is
safe for preservation and tone-map regression tests.

The generator is also the baseline for functional policy tests around HDR
settings:

```sh
scripts/generate-test-corpus /tmp/voom-issue-304-corpus
```

Real HDR10+ and Dolby Vision round-trip validation still needs external sample
fixtures because ffmpeg does not synthesize representative HDR10+ dynamic
metadata or Dolby Vision RPU data. Gate those tests on fixture availability plus
`hdr10plus_tool` or `dovi_tool`, then verify the output with the corresponding
tool extraction command.
