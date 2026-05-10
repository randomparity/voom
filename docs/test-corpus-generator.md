# Test Corpus Generator

`scripts/generate-test-corpus` creates synthetic media files for local and CI
testing. It writes generated media plus a `manifest.json` that records coverage
tags, expected media traits, skipped fixtures, failures, and corruptions.

## Text-to-Speech Fixtures

Speech fixtures generate video files with spoken audio tracks. They are useful
for testing transcription workflows, language filtering policies, and language
identification behavior without using copyrighted media.

Linux is the supported baseline for TTS generation. Install `espeak-ng` before
generating speech fixtures:

```sh
sudo apt-get install espeak-ng
```

macOS developers can use the built-in `say` command as a local fallback. The
macOS fallback is for convenience; Linux automation should use `espeak-ng`.

Generate the speech fixtures:

```sh
scripts/generate-test-corpus /tmp/voom-tts \
  --profile coverage \
  --only speech-english-aac,speech-spanish-aac,speech-dual-language,speech-mixed-language \
  --duration 3
```

Force the Linux backend:

```sh
scripts/generate-test-corpus /tmp/voom-tts \
  --profile coverage \
  --only speech-english-aac \
  --tts-backend espeak-ng
```

Test no-backend behavior:

```sh
scripts/generate-test-corpus /tmp/voom-tts \
  --profile coverage \
  --only speech-english-aac \
  --tts-backend none
```

When no backend is available, speech fixtures are skipped. Inspect
`manifest.json`:

```sh
jq '.skipped[] | select(.stem | startswith("speech-"))' /tmp/voom-tts/manifest.json
```

The manifest `expect` field records intended transcripts and language coverage.
It documents what the fixture was designed to contain; it is not a guarantee
that every transcription or language-detection engine will infer the same text
or language.
