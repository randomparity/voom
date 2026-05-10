# Functional Test Plan: Stream Metadata-Stable Processing

Issue: <https://github.com/randomparity/voom/issues/336>

## Goal

Verify that ffmpeg-backed transcode/containerize operations preserve stream
language tags, titles, and default/forced dispositions unless policy actions
explicitly change them.

## Corpus Setup

Generate fixtures with unknown languages, non-default subtitles, forced
subtitles, WebVTT subtitles, and multiple audio/subtitle tracks:

```sh
scripts/generate-test-corpus /tmp/voom-issue-336-corpus \
  --profile coverage \
  --only hevc-surround,multichannel-flac,vp9-opus,pillarbox-h264,mp4-text-subtitle \
  --duration 2 \
  --seed 336
```

Use `docs/examples/metadata-stable-transcode.voom` as the policy.

## Execution

Capture stream metadata before and after processing:

```sh
voom inspect /tmp/voom-issue-336-corpus --format json > /tmp/voom-issue-336-before.json
voom process /tmp/voom-issue-336-corpus \
  --policy docs/examples/metadata-stable-transcode.voom \
  --on-error continue \
  --workers 2
voom inspect /tmp/voom-issue-336-corpus --format json > /tmp/voom-issue-336-after.json
```

## Assertions

1. Stream `language` values are unchanged for tracks that survive processing,
   including `und`.
2. Stream `title` values are unchanged for tracks that survive processing.
3. Subtitle `is_default: false` does not drift to `true`.
4. Forced subtitle `is_forced: true` remains true.
5. Global metadata and chapters are copied from the source input.
6. Container-incompatible subtitle conversions are reported through existing
   plan safeguard/warning paths instead of silently changing metadata.

## Adversarial Review

- A preservation flag must not override a policy action such as `set_language`
  or `clear_forced`.
- Explicit stream metadata must use output stream indexes, not input indexes,
  after attachment streams are excluded from video transcode maps.
- Unknown language (`und`) is a real value and must be emitted explicitly.
- Empty stream titles must be emitted as `title=` so ffmpeg cannot synthesize a
  stale title from prior output metadata.
