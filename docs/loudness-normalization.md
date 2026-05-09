# Audio Loudness Normalization

VOOM can normalize audio tracks with ffmpeg's EBU R128 `loudnorm` filter.
Use this when a library has inconsistent dialogue or episode-to-episode
volume.

## Policy syntax

Normalize kept audio tracks:

```voom
policy "broadcast-loudness" {
  phase audio {
    keep audio where lang == eng and not commentary {
      normalize: ebu_r128 {
        target_lufs: -23
        true_peak_db: -1.0
        lra_max: 18
      }
    }
  }
}
```

Normalize synthesized compatibility audio:

```voom
policy "mobile-loudness" {
  phase audio {
    synthesize "AAC Stereo" {
      codec: aac
      channels: stereo
      source: prefer(lang == eng and not commentary)
      bitrate: "192k"
      normalize: mobile
    }
  }
}
```

## Presets

| Preset | Target | True peak | LRA |
| --- | ---: | ---: | ---: |
| `ebu_r128` | -23 LUFS | -1.0 dBTP | 18 |
| `ebu_r128_broadcast` | -23 LUFS | -1.0 dBTP | 18 |
| `streaming_movies` | -24 LUFS | -2.0 dBTP | unlimited |
| `streaming_music` | -14 LUFS | -1.0 dBTP | unlimited |
| `mobile` | -16 LUFS | -1.5 dBTP | unlimited |
| `voice_focused` | -19 LUFS | -1.0 dBTP | 7 |

Override any preset with `target_lufs`, `true_peak_db`, `lra_max`, or
`tolerance_lufs`.

## Reporting

Use:

```bash
voom report --loudness
```

The report shows measured audio tracks, average integrated LUFS, average true
peak, and files outside the -23 LUFS broadcast target by more than 0.5 LUFS.

## Functional Test Plan

Generate synthetic media with quiet and hot mixes:

```bash
scripts/generate-test-corpus /tmp/voom-loudness-corpus \
  --only loudness-quiet-dialogue,loudness-hot-mix
```

Then run a policy against `loudness-quiet-dialogue.mkv` and
`loudness-hot-mix.mkv`, verify ffmpeg executes a two-pass `loudnorm` flow, and
confirm the resulting audio measures within +/-0.5 LUFS of the selected target.

Adversarial cases to review:

- Already normalized input should skip standalone normalization actions.
- Invalid presets and non-numeric LUFS values should fail DSL validation.
- 5.1 and 7.1 tracks should preserve channel count unless policy also changes it.
- Hardware video encoding in the same plan must not drop audio filters.
