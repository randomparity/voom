# TTS Corpus Functional Test Plan

## Prerequisites

- `ffmpeg` and `ffprobe` on `PATH`
- Linux: `espeak-ng` on `PATH` for real speech generation
- Optional macOS local fallback: `say`

## Generate Speech Corpus

```sh
rm -rf /tmp/voom-tts
scripts/generate-test-corpus /tmp/voom-tts \
  --profile coverage \
  --only speech-english-aac,speech-spanish-aac,speech-dual-language,speech-mixed-language \
  --duration 3 \
  --tts-backend espeak-ng
```

Expected:

- `speech-english-aac.mp4` exists.
- `speech-spanish-aac.mp4` exists.
- `speech-dual-language.mkv` exists.
- `speech-mixed-language.mkv` exists.
- `manifest.json` reports four generated files and no failures.

## Inspect Manifest Intent

```sh
jq '.generated[] | {stem, covers, expect}' /tmp/voom-tts/manifest.json
```

Expected: each speech fixture has `expect.speech == true`, transcript text, and
`speech_languages` matching its fixture name.

## Verify Audio Track Metadata

```sh
ffprobe -v error -show_streams -select_streams a -of json \
  /tmp/voom-tts/speech-dual-language.mkv |
  jq '[.streams[] | {codec_name, channels, language: .tags.language, title: .tags.title}]'
```

Expected: two AAC stereo audio tracks, one `eng` and one `spa`.

## No-Backend Skip Behavior

```sh
rm -rf /tmp/voom-tts-none
scripts/generate-test-corpus /tmp/voom-tts-none \
  --profile coverage \
  --only speech-english-aac \
  --tts-backend none
jq '.summary, .skipped' /tmp/voom-tts-none/manifest.json
```

Expected: zero generated files, one skipped file, and a reason explaining that no
TTS backend is available.

## Example Policy Validation After Policy Examples Are Added

Run these commands after the speech example policies and test suites are
present:

```sh
cargo run -q -- policy validate docs/examples/speech-language-filter.voom
cargo run -q -- policy validate docs/examples/speech-transcription-check.voom
cargo run -q -- policy test docs/examples/tests/speech-language-filter.test.json
cargo run -q -- policy test docs/examples/tests/speech-transcription-check.test.json
```

Expected after the example policy files are present: all commands succeed.
