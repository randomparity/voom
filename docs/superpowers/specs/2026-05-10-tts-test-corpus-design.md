# Text-to-Speech Test Corpus Design

## Purpose

Issue 306 asks for `scripts/generate-test-corpus` to generate test video files
with spoken audio so VOOM workflows can exercise transcription and language
identification against deterministic, synthetic content. The current generator
already covers codecs, containers, loudness, subtitles, HDR, crop cases, and
corrupt files. This design adds speech fixtures without replacing those
mechanisms or adding speculative product behavior.

The feature must work on Linux because CI and most automation run there. macOS
developer convenience is useful, but Linux support is the baseline.

## Scope

This design changes:

- `scripts/generate-test-corpus`
- tests for the generator helpers and generated behavior
- corpus/user documentation
- example `.voom` policies that demonstrate language and transcription-oriented
  usage against the generated corpus
- plan and code review artifacts for adversarial review

This design does not change VOOM transcription plugins, language detection
algorithms, policy evaluation semantics, or media processing behavior outside
the test corpus generator.

## TTS Backend Strategy

Use an external command-line TTS backend to render temporary WAV files, then mux
those WAV files through the existing ffmpeg command-building flow.

Backend priority:

1. `espeak-ng`: supported Linux baseline. This is the backend functional tests
   should use when they need real speech output.
2. ffmpeg `flite` filter: optional fallback when a local ffmpeg build includes
   it.
3. macOS `say`: optional local developer fallback only. It is never assumed by
   Linux tests or documentation as the portable path.

If no backend is available, TTS fixtures skip cleanly. The run manifest records
the skipped files and a clear reason such as `TTS backend not available
(requires espeak-ng, ffmpeg flite, or macOS say)`.

Do not add a Python package dependency for TTS. The generator already depends on
external media tools, and a command-line backend keeps setup transparent.

## Fixture Model

Add a new deterministic fixture family, `build_tts_feature_specs()`, and include
it from `build_manifest()`.

Each speech audio track extends the existing audio spec shape:

```python
{
    "codec": "aac",
    "channels": 2,
    "lang": "eng",
    "source": "tts",
    "voice": "en-us",
    "text": "The quick brown fox confirms the English narration track.",
}
```

TTS fixture specs use normal `video`, `audio`, `subs`, `profiles`, `covers`,
and `expect` fields. The `expect` field records the intended transcript and
language coverage so tests can assert behavior from the manifest rather than
encoding fixture knowledge in test code.

Example manifest expectation:

```python
"expect": {
    "bad_file": False,
    "audio_tracks": 2,
    "speech": True,
    "speech_languages": ["eng", "spa"],
    "transcript": [
        {"track": 0, "lang": "eng", "text": "English narration ..."},
        {"track": 1, "lang": "spa", "text": "Narracion en espanol ..."},
    ],
}
```

## Deterministic Fixtures

Add four named fixtures:

- `speech-english-aac.mp4`: English speech, AAC stereo, smoke and coverage.
- `speech-spanish-aac.mp4`: Spanish speech, AAC stereo, coverage.
- `speech-dual-language.mkv`: separate English and Spanish speech tracks,
  coverage.
- `speech-mixed-language.mkv`: one speech track containing short English and
  Spanish segments, coverage.

Keep durations short. Speech fixtures should remain in the same runtime class as
the existing smoke and coverage corpus. If a backend renders a clip shorter than
the requested video duration, pad audio with silence instead of failing.

Do not add Japanese speech in the first implementation unless the selected
Linux backend and test environment can render it reliably. If Japanese coverage
is still desired after implementing the baseline, file a follow-up issue rather
than adding unreliable fixtures.

## Command Construction

The generator should create TTS WAV files inside its existing temporary
directory before building ffmpeg input options. The ffmpeg command maps each
rendered WAV as an audio input in the same order as existing generated audio
tracks.

Expected flow:

1. Detect the available TTS backend once per run.
2. While preparing a fixture, render each `source: "tts"` audio track to a WAV.
3. Use `-i <tmp_wav>` for those tracks instead of `-f lavfi -i <sine>`.
4. Continue to use existing codec options, metadata, dispositions, and manifest
   entry creation.
5. If rendering fails for a TTS fixture, mark that fixture failed with backend
   and track context.

Existing non-TTS audio sources, including `sine` and `dynamic_bursts`, keep
their current behavior.

## CLI Behavior

Do not add a required CLI flag. The deterministic profile selects TTS fixtures
like any other fixture.

Add one optional discovery command if it proves necessary during implementation:

```text
--tts-backend auto|espeak-ng|flite|say|none
```

Default is `auto`. `none` forces TTS fixtures to skip and is useful for tests.
Only add this flag if tests or user workflows need deterministic backend
selection; otherwise keep backend selection internal to avoid unnecessary
surface area.

## Documentation

Add user-facing documentation that explains:

- TTS fixture purpose.
- Linux setup with `espeak-ng`.
- macOS local fallback with `say`.
- How skipped TTS fixtures appear in `manifest.json`.
- How to generate only speech fixtures.
- How to use the generated corpus with language and transcription-oriented
  policies.

Update example policy documentation and add focused example policies:

- `docs/examples/speech-language-filter.voom`: demonstrates keeping preferred
  spoken languages and warning when English audio is absent.
- `docs/examples/speech-transcription-check.voom`: demonstrates the intended
  transcription workflow using generated speech fixtures and existing
  transcription-related plugin concepts.

Parser-validate any new example policy files and add tests if the repository's
example policy test suite does not pick them up automatically.

## Functional Test Plan

Create a functional test plan that uses `scripts/generate-test-corpus` to
generate testable content.

Required scenarios:

- Linux happy path with `espeak-ng` installed:
  `scripts/generate-test-corpus /tmp/voom-tts --profile coverage`
  `--only speech-english-aac,speech-dual-language --duration 3`
- No-backend path:
  force no backend if a CLI option exists, or mock backend discovery in tests;
  verify TTS fixtures are skipped with clear manifest reasons.
- Mixed-language fixture:
  generate `speech-mixed-language`, inspect `manifest.json`, and confirm the
  expected language segments and transcript text are present.
- Policy examples:
  validate new `.voom` policies and run dry-run or plan-only flows against the
  generated corpus where current VOOM capabilities support it.

Automated tests should cover pure helper behavior first:

- backend discovery priority
- backend command construction
- TTS WAV path creation
- ffmpeg input mapping for mixed TTS and lavfi audio
- manifest expectations for all speech fixtures
- clean skip entries when no backend is available

Functional tests that require real `espeak-ng` should be written so they can be
skipped with an explicit message when the binary is absent.

## Adversarial Review Requirements

The implementation plan must include adversarial review checkpoints before code
is merged:

- Plan review: check whether the design has hidden platform assumptions,
  unnecessary CLI surface area, unreliable language coverage, or weak test
  evidence.
- Code review: check command injection boundaries, temporary file cleanup,
  backend failure reporting, fixture runtime, manifest consistency, and whether
  Linux support is genuinely tested.
- Documentation review: verify docs do not promise unavailable transcription
  behavior and clearly distinguish generated fixture intent from automatic
  language detection accuracy.

Any valid issue found during adversarial review should be fixed before the PR is
merged. If it is intentionally deferred, file a new GitHub issue and continue
the requested issue loop for that follow-up.

## Acceptance Criteria

- The generator can produce real speech fixtures on Linux with `espeak-ng`.
- The generator skips TTS fixtures cleanly when no supported backend exists.
- Existing non-TTS fixture generation behavior remains unchanged.
- `manifest.json` records speech fixture expectations and skip/failure context.
- New helper and functional tests cover backend selection, command generation,
  fixture manifest data, and no-backend behavior.
- User documentation explains how to generate and use speech fixtures.
- Example policy files demonstrate the feature and validate successfully.
- The implementation is split into small conventional commits.
- The PR includes adversarial review notes and no deferred work without a linked
  follow-up issue.
