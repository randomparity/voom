# Issue 306 Adversarial Review

## Plan Review

- Platform assumption check: PASS. The plan makes Linux the supported baseline
  with `espeak-ng` and treats macOS `say` as a convenience fallback, so the design
  does not depend on macOS-only tooling.
- CLI surface area check: PASS. The only new CLI surface is optional
  `--tts-backend auto|espeak-ng|flite|say|none`; tests and documentation need it
  for deterministic backend selection and no-backend skip coverage.
- Language coverage reliability check: PASS. The plan intentionally limits the
  first fixture set to English and Spanish, explicitly defers Japanese unless the
  Linux backend can render it reliably, and frames mixed-language expectations as
  fixture intent.
- Test evidence check: PASS with one caveat. Tests cover backend discovery,
  command argv construction, TTS fixture manifest data, no-backend skip behavior,
  and an `espeak-ng` functional path that skips when the binary is absent. This
  worktree cannot prove real Linux speech generation because `espeak-ng` is not
  installed locally.

## Code Review

- Command injection boundaries: PASS. TTS rendering and ffmpeg execution use
  `subprocess.run()` with list argv, not shell strings. The flite fallback passes
  the lavfi expression as a single argv item and does not invoke a shell.
- Temporary file cleanup: PASS. Rendered TTS WAV files are written under the
  existing `tempfile.TemporaryDirectory(prefix="voom-corpus-")`, which is scoped
  to normal fixture generation and removed after the run.
- Backend failure reporting: PASS. Missing explicitly requested backends exit
  with actionable stderr, unavailable auto backends skip TTS fixtures with a
  manifest reason, and render failures include the backend name and stderr in the
  failed fixture reason.
- Fixture runtime: PASS after fix. Speech fixture duration is short and TTS/ffmpeg
  subprocesses have timeouts. I also split the 152-line TTS fixture builder into
  smaller helpers so the new code no longer violates the project function-length
  limit.
- Manifest consistency: PASS. Generated, skipped, and failed speech entries carry
  the same `expect` payload, including transcript intent and language coverage;
  tests assert both generated fixture expectations and no-backend skip entries.
- Linux support evidence: PASS with caveat. The code prefers `espeak-ng`, docs
  specify it as the Linux backend, and tests exercise the real path when
  installed. Local verification will report the `espeak-ng` generation test as
  skipped in this worktree because the binary is absent.

## Documentation Review

- TTS setup accuracy: PASS. The docs identify `ffmpeg`/`ffprobe` prerequisites,
  Linux `espeak-ng` setup, and macOS `say` as an optional local fallback.
- Transcription promise boundary: PASS. `docs/test-corpus-generator.md` states
  that manifest transcript text and language coverage are fixture intent, not a
  guarantee that every recognizer or language detector will infer the same output.
- Manifest expectation clarity: PASS. The functional plan tells readers to
  inspect `expect` as manifest intent and check concrete generated/skipped
  summary fields rather than relying on recognizer output.
- Example policy accuracy: PASS. The speech example policies operate on declared
  track language metadata and retention/tagging behavior, not unimplemented
  transcription APIs, and they have policy test suites.

## Outcomes

- Fixed before merge: Split `build_tts_feature_specs()` into focused helper
  functions and made the added `tts_backend` parameter keyword-only on
  `build_ffmpeg_cmd()` so new code stays within project limits for function
  length and positional parameters.
- Deferred follow-up issues: None.
