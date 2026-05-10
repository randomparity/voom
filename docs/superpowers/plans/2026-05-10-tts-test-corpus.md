# Text-to-Speech Test Corpus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Linux-supported text-to-speech fixtures to
`scripts/generate-test-corpus` for transcription and language-identification
testing.

**Architecture:** Keep `scripts/generate-test-corpus` as the single corpus entry
point. Add a small TTS backend layer that renders temporary WAV files with
`espeak-ng`, optional ffmpeg `flite`, or optional macOS `say`, then reuses the
existing ffmpeg muxing, metadata, profiles, and manifest paths. Speech fixtures
are ordinary deterministic specs with explicit `expect` transcript metadata and
clean skip behavior when no backend is available.

**Tech Stack:** Python 3 standard library, ffmpeg/ffprobe, optional
`espeak-ng`, optional ffmpeg `flite`, optional macOS `say`, pytest, Rust
policy/example validation via Cargo.

---

## File Structure

- Modify `scripts/generate-test-corpus`: TTS backend detection, TTS fixture
  specs, temporary WAV rendering, ffmpeg input mapping, CLI backend override,
  manifest skip/failure context.
- Modify `tests/scripts/test_generate_test_corpus.py`: pure helper tests for
  fixture specs, backend discovery, command construction, input mapping, skips,
  and functional generation when `espeak-ng` is installed.
- Create `docs/test-corpus-generator.md`: user-facing generator documentation
  focused on TTS setup, generation, manifest output, and troubleshooting.
- Modify `docs/INDEX.md`: link the new generator documentation.
- Create `docs/functional-test-plan-tts-corpus.md`: manual/functional test
  plan that generates speech fixtures with `scripts/generate-test-corpus`.
- Create `docs/examples/speech-language-filter.voom`: language-filtering
  example policy for generated speech fixtures.
- Create `docs/examples/speech-transcription-check.voom`: transcription-oriented
  example policy using existing plugin concepts without promising automatic
  accuracy.
- Modify `docs/examples/README.md`: document the two new example policies.
- Create `docs/examples/tests/speech-language-filter.test.json`: parser/policy
  test suite for the language-filter example.
- Create `docs/examples/tests/speech-transcription-check.test.json`: parser/policy
  test suite for the transcription-oriented example.
- Create `docs/examples/tests/speech-dual-language.json`: JSON policy fixture
  modelling generated dual-language speech tracks.
- Create `docs/examples/tests/speech-spanish-only.json`: JSON policy fixture
  modelling generated Spanish-only speech tracks.
- Create `docs/plans/issue-306-adversarial-review.md`: plan/code/docs review
  notes and outcomes before PR.

Do not add Python dependencies. Do not split `scripts/generate-test-corpus` into
modules for this feature.

## Task 1: Add Failing Tests For TTS Fixtures And Backend Selection

**Files:**
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Add tests for TTS fixture specs**

Append these tests after `test_build_manifest_includes_required_corrupt_fixtures`:

```python
def test_build_manifest_includes_tts_fixtures(generator):
    specs = {spec["stem"]: spec for spec in generator.build_manifest()}

    assert {
        "speech-english-aac",
        "speech-spanish-aac",
        "speech-dual-language",
        "speech-mixed-language",
    }.issubset(specs)
    assert specs["speech-english-aac"]["profiles"] == ["smoke", "coverage"]
    assert specs["speech-dual-language"]["expect"]["speech_languages"] == [
        "eng",
        "spa",
    ]
    assert specs["speech-mixed-language"]["expect"]["speech"] is True
    assert "audio.speech.mixed_language" in specs["speech-mixed-language"]["covers"]
```

- [ ] **Step 2: Add backend discovery tests**

Append these tests near the ffmpeg availability tests:

```python
def test_discover_tts_backend_prefers_espeak_ng(generator, monkeypatch):
    def fake_which(name):
        return {
            "espeak-ng": "/usr/bin/espeak-ng",
            "say": "/usr/bin/say",
        }.get(name)

    monkeypatch.setattr(generator.shutil, "which", fake_which)
    monkeypatch.setattr(generator, "ffmpeg_supports_flite", lambda: True)

    backend = generator.discover_tts_backend("auto")

    assert backend == generator.TtsBackend("espeak-ng", "/usr/bin/espeak-ng")


def test_discover_tts_backend_uses_flite_before_say(generator, monkeypatch):
    def fake_which(name):
        return "/usr/bin/say" if name == "say" else None

    monkeypatch.setattr(generator.shutil, "which", fake_which)
    monkeypatch.setattr(generator, "ffmpeg_supports_flite", lambda: True)

    backend = generator.discover_tts_backend("auto")

    assert backend == generator.TtsBackend("flite", "ffmpeg")


def test_discover_tts_backend_none_returns_none(generator, monkeypatch):
    monkeypatch.setattr(generator.shutil, "which", lambda name: None)
    monkeypatch.setattr(generator, "ffmpeg_supports_flite", lambda: False)

    assert generator.discover_tts_backend("none") is None
    assert generator.discover_tts_backend("auto") is None
```

- [ ] **Step 3: Add backend override tests**

Append:

```python
def test_discover_tts_backend_requested_missing_backend_exits(
    generator, monkeypatch, capsys
):
    monkeypatch.setattr(generator.shutil, "which", lambda name: None)
    monkeypatch.setattr(generator, "ffmpeg_supports_flite", lambda: False)

    with pytest.raises(SystemExit) as exc_info:
        generator.discover_tts_backend("espeak-ng")

    assert exc_info.value.code == 1
    assert (
        capsys.readouterr().err
        == "Error: requested TTS backend 'espeak-ng' is not available\n"
    )
```

- [ ] **Step 4: Run tests and verify failure**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py \
  -k "tts or discover_tts"
```

Expected: FAIL because `TtsBackend`, `discover_tts_backend()`, and TTS fixture
specs do not exist yet.

- [ ] **Step 5: Commit failing tests**

```bash
git add tests/scripts/test_generate_test_corpus.py
git commit -m "test: cover TTS corpus fixture planning"
```

## Task 2: Implement TTS Fixture Specs And Backend Discovery

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Add backend constants and dataclass**

Add near the profile constants:

```python
TTS_BACKEND_CHOICES = ("auto", "espeak-ng", "flite", "say", "none")
TTS_UNAVAILABLE_REASON = (
    "TTS backend not available (requires espeak-ng, ffmpeg flite, or macOS say)"
)
```

Add near the existing dataclasses:

```python
@dataclass(frozen=True)
class TtsBackend:
    """A discovered command-line TTS backend."""

    name: str
    command: str
```

- [ ] **Step 2: Add TTS fixture specs**

Add this helper before `build_manifest()`:

```python
def transcript_entry(track, lang, text):
    """Return a stable transcript expectation entry."""
    return {"track": track, "lang": lang, "text": text}


def build_tts_feature_specs():
    """Return speech fixture specs for transcription and language testing."""
    english = "This English narration confirms the speech test fixture."
    spanish = "Esta narracion en espanol confirma la pista de voz."
    mixed = (
        "This English segment starts the mixed language fixture. "
        "Esta frase en espanol comprueba el cambio de idioma."
    )

    base_video = {
        "source": "testsrc2",
        "codec": "libx264",
        "size": "1280x720",
        "fps": 24,
    }
    return [
        {
            "stem": "speech-english-aac",
            "ext": "mp4",
            "profiles": ["smoke", "coverage"],
            "covers": ["audio.speech.tts", "audio.speech.language.eng"],
            "expect": {
                "bad_file": False,
                "audio_tracks": 1,
                "speech": True,
                "speech_languages": ["eng"],
                "transcript": [transcript_entry(0, "eng", english)],
            },
            "video": dict(base_video),
            "audio": [
                {
                    "codec": "aac",
                    "channels": 2,
                    "lang": "eng",
                    "source": "tts",
                    "voice": "en-us",
                    "text": english,
                }
            ],
            "subs": [],
            "special": [],
            "requires_tts": True,
        },
        {
            "stem": "speech-spanish-aac",
            "ext": "mp4",
            "profiles": ["coverage"],
            "covers": ["audio.speech.tts", "audio.speech.language.spa"],
            "expect": {
                "bad_file": False,
                "audio_tracks": 1,
                "speech": True,
                "speech_languages": ["spa"],
                "transcript": [transcript_entry(0, "spa", spanish)],
            },
            "video": dict(base_video),
            "audio": [
                {
                    "codec": "aac",
                    "channels": 2,
                    "lang": "spa",
                    "source": "tts",
                    "voice": "es",
                    "text": spanish,
                }
            ],
            "subs": [],
            "special": [],
            "requires_tts": True,
        },
        {
            "stem": "speech-dual-language",
            "ext": "mkv",
            "profiles": ["coverage"],
            "covers": [
                "audio.speech.tts",
                "audio.speech.language.eng",
                "audio.speech.language.spa",
                "audio.speech.dual_track",
            ],
            "expect": {
                "bad_file": False,
                "audio_tracks": 2,
                "speech": True,
                "speech_languages": ["eng", "spa"],
                "transcript": [
                    transcript_entry(0, "eng", english),
                    transcript_entry(1, "spa", spanish),
                ],
            },
            "video": dict(base_video),
            "audio": [
                {
                    "codec": "aac",
                    "channels": 2,
                    "lang": "eng",
                    "source": "tts",
                    "voice": "en-us",
                    "text": english,
                    "title": "English Speech",
                },
                {
                    "codec": "aac",
                    "channels": 2,
                    "lang": "spa",
                    "source": "tts",
                    "voice": "es",
                    "text": spanish,
                    "title": "Spanish Speech",
                },
            ],
            "subs": [],
            "special": [],
            "requires_tts": True,
        },
        {
            "stem": "speech-mixed-language",
            "ext": "mkv",
            "profiles": ["coverage"],
            "covers": [
                "audio.speech.tts",
                "audio.speech.language.eng",
                "audio.speech.language.spa",
                "audio.speech.mixed_language",
            ],
            "expect": {
                "bad_file": False,
                "audio_tracks": 1,
                "speech": True,
                "speech_languages": ["eng", "spa"],
                "transcript": [transcript_entry(0, "mul", mixed)],
                "segments": [
                    {"lang": "eng", "text": "This English segment starts"},
                    {"lang": "spa", "text": "Esta frase en espanol"},
                ],
            },
            "video": dict(base_video),
            "audio": [
                {
                    "codec": "aac",
                    "channels": 2,
                    "lang": "mul",
                    "source": "tts",
                    "voice": "en-us",
                    "text": mixed,
                }
            ],
            "subs": [],
            "special": [],
            "requires_tts": True,
        },
    ]
```

Update `build_manifest()`:

```python
        + build_tts_feature_specs()
        + build_corrupt_fixture_specs()
```

- [ ] **Step 3: Add backend discovery helpers**

Add near `probe_encoders()`:

```python
def ffmpeg_supports_flite():
    """Return True when the local ffmpeg build has the flite source filter."""
    try:
        result = subprocess.run(
            ["ffmpeg", "-hide_banner", "-h", "filter=flite"],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except Exception:
        return False
    output = f"{result.stdout}\n{result.stderr}"
    return result.returncode == 0 and "Unknown filter 'flite'" not in output


def discover_tts_backend(choice):
    """Return the selected TTS backend, or None when TTS should be skipped."""
    if choice == "none":
        return None
    if choice in {"auto", "espeak-ng"} and shutil.which("espeak-ng"):
        return TtsBackend("espeak-ng", shutil.which("espeak-ng"))
    if choice == "espeak-ng":
        print(
            "Error: requested TTS backend 'espeak-ng' is not available",
            file=sys.stderr,
        )
        sys.exit(1)
    if choice in {"auto", "flite"} and ffmpeg_supports_flite():
        return TtsBackend("flite", "ffmpeg")
    if choice == "flite":
        print("Error: requested TTS backend 'flite' is not available", file=sys.stderr)
        sys.exit(1)
    if choice in {"auto", "say"} and shutil.which("say"):
        return TtsBackend("say", shutil.which("say"))
    if choice == "say":
        print("Error: requested TTS backend 'say' is not available", file=sys.stderr)
        sys.exit(1)
    return None
```

Refactor repeated `shutil.which("espeak-ng")` in a follow-up cleanup inside this
task if Ruff flags it.

- [ ] **Step 4: Add CLI option and generation option field**

In `build_arg_parser()` add:

```python
    parser.add_argument(
        "--tts-backend",
        choices=TTS_BACKEND_CHOICES,
        default="auto",
        help="TTS backend for speech fixtures (default: auto)",
    )
```

Add to `GenerationOptions`:

```python
    tts_backend: TtsBackend | None = None
```

In `main()`, after encoder probing:

```python
    tts_backend = discover_tts_backend(args.tts_backend)
```

Pass `tts_backend=tts_backend` into `GenerationOptions(...)`.

- [ ] **Step 5: Run focused tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py \
  -k "tts or discover_tts"
```

Expected: tests from Task 1 pass.

- [ ] **Step 6: Run formatter/checker for touched Python**

Run:

```bash
uv run ruff format scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: clean output.

- [ ] **Step 7: Commit implementation**

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "feat: define TTS corpus fixtures"
```

## Task 3: Add TTS Rendering And FFmpeg Input Mapping

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Add failing tests for render commands and input mapping**

Append near `test_build_audio_input_dynamic_bursts_uses_aevalsrc_expression`:

```python
def test_build_tts_command_uses_espeak_ng(generator, tmp_path):
    backend = generator.TtsBackend("espeak-ng", "/usr/bin/espeak-ng")
    out = tmp_path / "speech.wav"
    cmd = generator.build_tts_command(
        backend,
        {"text": "hello world", "voice": "en-us"},
        out,
    )

    assert cmd == [
        "/usr/bin/espeak-ng",
        "-v",
        "en-us",
        "-w",
        str(out),
        "hello world",
    ]


def test_build_tts_command_uses_say(generator, tmp_path):
    backend = generator.TtsBackend("say", "/usr/bin/say")
    out = tmp_path / "speech.wav"
    cmd = generator.build_tts_command(backend, {"text": "hello", "voice": "Alex"}, out)

    assert cmd == ["/usr/bin/say", "-v", "Alex", "-o", str(out), "--data-format=LEF32@22050", "hello"]


def test_prepare_audio_inputs_renders_tts_and_keeps_lavfi(generator, monkeypatch, tmp_path):
    backend = generator.TtsBackend("espeak-ng", "/usr/bin/espeak-ng")
    rendered = []

    def fake_render(audio, path, selected_backend):
        rendered.append((audio["text"], path.name, selected_backend.name))
        path.write_bytes(b"RIFFfake")

    monkeypatch.setattr(generator, "render_tts_audio", fake_render)

    spec = {
        "stem": "mixed",
        "audio": [
            {
                "codec": "aac",
                "channels": 2,
                "source": "tts",
                "voice": "en-us",
                "text": "speech",
            },
            {"codec": "aac", "channels": 2},
        ],
    }

    inputs = generator.prepare_audio_inputs(spec, duration=2, tmpdir=tmp_path, tts_backend=backend)

    assert rendered == [("speech", "tts_mixed_0.wav", "espeak-ng")]
    assert inputs == [
        {"kind": "file", "value": str(tmp_path / "tts_mixed_0.wav")},
        {
            "kind": "lavfi",
            "value": "sine=frequency=550:duration=2:sample_rate=48000",
        },
    ]
```

Wrap the long `say` assertion and `prepare_audio_inputs(...)` call before
committing if Ruff formatting does not.

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py \
  -k "build_tts_command or prepare_audio_inputs"
```

Expected: FAIL because `build_tts_command()` and `prepare_audio_inputs()` do not
exist.

- [ ] **Step 3: Implement TTS command builders**

Add near `build_audio_input()`:

```python
def build_tts_command(backend, audio, output_path):
    """Build the command that renders one TTS track to a WAV file."""
    text = audio["text"]
    voice = audio.get("voice")
    if backend.name == "espeak-ng":
        cmd = [backend.command]
        if voice:
            cmd += ["-v", voice]
        cmd += ["-w", str(output_path), text]
        return cmd
    if backend.name == "say":
        cmd = [backend.command]
        if voice:
            cmd += ["-v", voice]
        cmd += ["-o", str(output_path), "--data-format=LEF32@22050", text]
        return cmd
    raise ValueError(f"unsupported command TTS backend '{backend.name}'")
```

Add ffmpeg flite rendering separately because it uses ffmpeg directly:

```python
def build_flite_command(audio, output_path):
    """Build an ffmpeg command that renders one flite TTS track."""
    voice = audio.get("voice", "kal")
    text = (
        audio["text"]
        .replace("\\", "\\\\")
        .replace("'", "\\'")
        .replace(":", "\\:")
    )
    return [
        "ffmpeg",
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        f"flite=text='{text}':voice={voice}",
        str(output_path),
    ]
```

Add renderer:

```python
def render_tts_audio(audio, output_path, backend):
    """Render one TTS audio track to a temporary WAV file."""
    if backend is None:
        raise RuntimeError(TTS_UNAVAILABLE_REASON)
    if backend.name == "flite":
        cmd = build_flite_command(audio, output_path)
    else:
        cmd = build_tts_command(backend, audio, output_path)
    result = subprocess.run(cmd, capture_output=True, timeout=60)
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"TTS backend '{backend.name}' failed: {stderr}")
```

- [ ] **Step 4: Add audio input preparation**

Replace direct audio input loop in `build_input_options()` with a helper.

Add:

```python
def prepare_audio_inputs(spec, duration, tmpdir, tts_backend):
    """Return prepared audio inputs for a fixture spec."""
    inputs = []
    for i, audio in enumerate(spec["audio"]):
        if audio.get("source") == "tts":
            wav_path = Path(tmpdir) / f"tts_{spec['stem']}_{i}.wav"
            render_tts_audio(audio, wav_path, tts_backend)
            inputs.append({"kind": "file", "value": str(wav_path)})
        else:
            inputs.append(
                {
                    "kind": "lavfi",
                    "value": build_audio_input(audio, i, duration),
                }
            )
    return inputs
```

Change `build_input_options()` signature:

```python
def build_input_options(spec, duration, tmpdir, tts_backend=None):
```

Change the audio input section:

```python
    audio_inputs = prepare_audio_inputs(spec, duration, tmpdir, tts_backend)
    for audio_input in audio_inputs:
        if audio_input["kind"] == "lavfi":
            cmd += ["-f", "lavfi", "-i", audio_input["value"]]
        else:
            cmd += ["-i", audio_input["value"]]
```

Change `build_ffmpeg_cmd()` signature:

```python
def build_ffmpeg_cmd(spec, dest, duration, rng, tmpdir, tts_backend=None):
```

Pass `tts_backend` to `build_input_options(...)`.

Pass `options.tts_backend` from `generate_normal_spec()` into
`build_ffmpeg_cmd(...)`.

- [ ] **Step 5: Add skip behavior for unavailable TTS backend**

In `generate_normal_spec()` after encoder requirement handling:

```python
    if spec.get("requires_tts") and options.tts_backend is None:
        print(f"[{idx}/{options.total}] Skipping {filename} - {TTS_UNAVAILABLE_REASON}")
        result.skipped_entries.append(
            build_issue_entry(spec, filename, TTS_UNAVAILABLE_REASON)
        )
        result.skip_count += 1
        return
```

Use a plain ASCII hyphen in the new message.

- [ ] **Step 6: Run focused tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py \
  -k "tts or speech or prepare_audio_inputs"
```

Expected: all selected tests pass.

- [ ] **Step 7: Run dry-run smoke command**

Run:

```bash
scripts/generate-test-corpus /tmp/voom-tts-dry --profile coverage \
  --only speech-english-aac,speech-dual-language --duration 3 --dry-run
```

Expected: command output includes the speech fixture filenames. If no TTS backend
is available, generated commands should not be built for skipped TTS fixtures.

- [ ] **Step 8: Format, lint, and commit**

Run:

```bash
uv run ruff format scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: clean output.

Commit:

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "feat: render TTS audio for corpus fixtures"
```

## Task 4: Add Functional Tests For Real And Skipped TTS Generation

**Files:**
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Add CLI functional tests**

Append near the bottom:

```python
def test_tts_fixture_skips_when_backend_disabled(tmp_path):
    dest = tmp_path / "corpus"
    result = subprocess.run(
        [
            str(SCRIPT_PATH),
            str(dest),
            "--profile",
            "coverage",
            "--only",
            "speech-english-aac",
            "--tts-backend",
            "none",
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0
    manifest = json.loads((dest / "manifest.json").read_text())
    assert manifest["summary"] == {
        "generated": 0,
        "skipped": 1,
        "failed": 0,
        "corrupted": 0,
    }
    assert manifest["skipped"][0]["stem"] == "speech-english-aac"
    assert manifest["skipped"][0]["reason"] == generator.TTS_UNAVAILABLE_REASON
```

This test needs `json` and `subprocess` imports at the top of the test file.
Because it references `generator`, make it accept the fixture:

```python
def test_tts_fixture_skips_when_backend_disabled(generator, tmp_path):
```

- [ ] **Step 2: Add real `espeak-ng` functional test**

Append:

```python
def test_tts_fixture_generates_with_espeak_ng(generator, tmp_path):
    if generator.shutil.which("espeak-ng") is None:
        pytest.skip("espeak-ng not installed")

    dest = tmp_path / "corpus"
    result = subprocess.run(
        [
            str(SCRIPT_PATH),
            str(dest),
            "--profile",
            "coverage",
            "--only",
            "speech-english-aac",
            "--duration",
            "3",
            "--tts-backend",
            "espeak-ng",
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    output = dest / "speech-english-aac.mp4"
    manifest = json.loads((dest / "manifest.json").read_text())
    assert output.exists()
    assert manifest["summary"]["generated"] == 1
    assert manifest["generated"][0]["expect"]["speech"] is True
    assert manifest["generated"][0]["expect"]["speech_languages"] == ["eng"]
```

- [ ] **Step 3: Run new functional tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py \
  -k "tts_fixture"
```

Expected: skip-disabled test passes. Real `espeak-ng` test passes when installed
and is skipped with `espeak-ng not installed` otherwise.

- [ ] **Step 4: Run all generator tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: all tests pass.

- [ ] **Step 5: Format, lint, and commit**

Run:

```bash
uv run ruff format tests/scripts/test_generate_test_corpus.py
uv run ruff check tests/scripts/test_generate_test_corpus.py
```

Expected: clean output.

Commit:

```bash
git add tests/scripts/test_generate_test_corpus.py
git commit -m "test: verify TTS corpus generation paths"
```

## Task 5: Add User Documentation And Functional Test Plan

**Files:**
- Create `docs/test-corpus-generator.md`
- Create `docs/functional-test-plan-tts-corpus.md`
- Modify `docs/INDEX.md`

- [ ] **Step 1: Create user-facing corpus generator docs**

Create `docs/test-corpus-generator.md`:

```markdown
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
```

Wrap the long `--only` line if markdown line checks complain.

- [ ] **Step 2: Link docs from index**

Add this row to the table in `docs/INDEX.md` after `policy-testing.md`:

```markdown
- [Test Corpus Generator](test-corpus-generator.md)
```

- [ ] **Step 3: Add functional test plan**

Create `docs/functional-test-plan-tts-corpus.md`:

```markdown
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

## Example Policy Validation

```sh
cargo run -q -- policy validate docs/examples/speech-language-filter.voom
cargo run -q -- policy validate docs/examples/speech-transcription-check.voom
cargo run -q -- policy test docs/examples/tests/speech-language-filter.test.json
cargo run -q -- policy test docs/examples/tests/speech-transcription-check.test.json
```

Expected: all commands succeed.
```

- [ ] **Step 4: Run markdown self-checks**

Run:

```bash
rg -n "PLACEHOLDER|unresolved work" docs/test-corpus-generator.md \
  docs/functional-test-plan-tts-corpus.md
awk 'length($0) > 120 { print FILENAME ":" FNR ":" length($0) ":" $0 }' \
  docs/test-corpus-generator.md docs/functional-test-plan-tts-corpus.md
```

Expected: no placeholders. Investigate any very long lines before committing.

- [ ] **Step 5: Commit docs**

```bash
git add docs/test-corpus-generator.md docs/functional-test-plan-tts-corpus.md docs/INDEX.md
git commit -m "docs: explain TTS corpus generation"
```

## Task 6: Add Example Policies And Policy Tests

**Files:**
- Create `docs/examples/speech-language-filter.voom`
- Create `docs/examples/speech-transcription-check.voom`
- Modify `docs/examples/README.md`
- Create `docs/examples/tests/speech-dual-language.json`
- Create `docs/examples/tests/speech-spanish-only.json`
- Create `docs/examples/tests/speech-language-filter.test.json`
- Create `docs/examples/tests/speech-transcription-check.test.json`
- Modify `crates/voom-dsl/tests/parser_snapshots.rs`

- [ ] **Step 1: Create language-filter example policy**

Create `docs/examples/speech-language-filter.voom`:

```voom
// Speech fixture language filtering policy.
//
// Designed for generated files from scripts/generate-test-corpus:
// - speech-english-aac
// - speech-spanish-aac
// - speech-dual-language
// - speech-mixed-language

policy "speech-language-filter" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
    on_error: continue
  }

  phase keep-english {
    keep audio where lang in [eng, und, mul]
  }

  phase validate {
    depends_on: [keep-english]

    when not exists(audio where lang == eng or lang == mul) {
      warn "No English or mixed-language speech audio in {filename}"
    }
  }
}
```

- [ ] **Step 2: Create transcription-oriented example policy**

Create `docs/examples/speech-transcription-check.voom`:

```voom
// Speech fixture transcription workflow policy.
//
// This policy demonstrates how generated TTS fixtures can drive workflows that
// depend on transcription metadata. The generated corpus records intended
// transcript text in manifest.json; actual transcription accuracy depends on
// the configured transcription plugin.

policy "speech-transcription-check" {
  config {
    languages audio: [eng, spa]
    languages subtitle: [eng]
    on_error: continue
  }

  phase classify-speech {
    rules all {
      rule "english-speech" {
        when exists(audio where lang == eng) {
          set_tag "speech_language" "eng"
        }
      }

      rule "spanish-speech" {
        when exists(audio where lang == spa) {
          set_tag "speech_language_alt" "spa"
        }
      }

      rule "mixed-speech" {
        when exists(audio where lang == mul) {
          set_tag "speech_language_mixed" "true"
          warn "Mixed-language speech fixture detected in {filename}"
        }
      }
    }
  }

  phase validate {
    depends_on: [classify-speech]

    when count(audio) == 0 {
      fail "No speech audio tracks remain in {filename}"
    }
  }
}
```

- [ ] **Step 3: Add policy fixtures**

Create `docs/examples/tests/speech-dual-language.json`:

```json
{
  "path": "/media/speech-dual-language.mkv",
  "container": "Mkv",
  "duration": 3.0,
  "size": 100000,
  "tracks": [
    {
      "index": 0,
      "track_type": "Video",
      "codec": "h264",
      "language": "und",
      "title": "",
      "is_default": true,
      "is_forced": false,
      "channels": null,
      "channel_layout": null,
      "sample_rate": null,
      "bit_depth": null,
      "width": 1280,
      "height": 720,
      "frame_rate": 24.0,
      "is_vfr": false,
      "is_hdr": false,
      "hdr_format": null,
      "pixel_format": null
    },
    {
      "index": 1,
      "track_type": "AudioMain",
      "codec": "aac",
      "language": "eng",
      "title": "English Speech",
      "is_default": true,
      "is_forced": false,
      "channels": 2,
      "channel_layout": "stereo",
      "sample_rate": 48000,
      "bit_depth": null
    },
    {
      "index": 2,
      "track_type": "AudioAlternate",
      "codec": "aac",
      "language": "spa",
      "title": "Spanish Speech",
      "is_default": false,
      "is_forced": false,
      "channels": 2,
      "channel_layout": "stereo",
      "sample_rate": 48000,
      "bit_depth": null
    }
  ]
}
```

Create `docs/examples/tests/speech-spanish-only.json` by copying the structure
above with path `/media/speech-spanish-aac.mp4`, container `Mp4`, one video
track, and one audio track:

```json
{
  "path": "/media/speech-spanish-aac.mp4",
  "container": "Mp4",
  "duration": 3.0,
  "size": 80000,
  "tracks": [
    {
      "index": 0,
      "track_type": "Video",
      "codec": "h264",
      "language": "und",
      "title": "",
      "is_default": true,
      "is_forced": false,
      "channels": null,
      "channel_layout": null,
      "sample_rate": null,
      "bit_depth": null,
      "width": 1280,
      "height": 720,
      "frame_rate": 24.0,
      "is_vfr": false,
      "is_hdr": false,
      "hdr_format": null,
      "pixel_format": null
    },
    {
      "index": 1,
      "track_type": "AudioMain",
      "codec": "aac",
      "language": "spa",
      "title": "Spanish Speech",
      "is_default": true,
      "is_forced": false,
      "channels": 2,
      "channel_layout": "stereo",
      "sample_rate": 48000,
      "bit_depth": null
    }
  ]
}
```

- [ ] **Step 4: Add policy test suites**

Create `docs/examples/tests/speech-language-filter.test.json`:

```json
{
  "policy": "../speech-language-filter.voom",
  "cases": [
    {
      "name": "keeps english track from dual-language speech fixture",
      "fixture": "speech-dual-language.json",
      "expect": {
        "phases_run": ["keep-english", "validate"],
        "audio_tracks_kept": 1,
        "no_warnings": true
      }
    },
    {
      "name": "warns when only spanish speech remains",
      "fixture": "speech-spanish-only.json",
      "expect": {
        "phases_run": ["keep-english", "validate"],
        "audio_tracks_kept": 0
      }
    }
  ]
}
```

Create `docs/examples/tests/speech-transcription-check.test.json`:

```json
{
  "policy": "../speech-transcription-check.voom",
  "cases": [
    {
      "name": "tags dual-language speech fixture",
      "fixture": "speech-dual-language.json",
      "expect": {
        "phases_run": ["classify-speech", "validate"],
        "no_warnings": true
      }
    }
  ]
}
```

- [ ] **Step 5: Add examples to README**

Add sections to `docs/examples/README.md`:

```markdown
### [speech-language-filter.voom](speech-language-filter.voom)
Generated speech corpus language filtering. Demonstrates keeping English and
mixed-language speech tracks while warning on Spanish-only files.

**Plugins used:** policy-evaluator

### [speech-transcription-check.voom](speech-transcription-check.voom)
Transcription-oriented speech fixture workflow. Demonstrates tagging generated
speech files by declared audio language and validating that speech audio remains.

**Plugins used:** policy-evaluator, optional whisper-transcriber workflows
```

Add these rows to the feature coverage table:

```markdown
| `keep` / `remove` | movie-library, anime, attachment, strict, full, speech-language-filter |
| `exists()` | anime, attachment, transcode, full, speech-language-filter, speech-transcription-check |
| `count()` | movie-library, anime, attachment, strict, full, speech-transcription-check |
| `set_tag` | metadata, full, speech-transcription-check |
| `skip` / `fail` / `warn` | movie-library, anime, strict, metadata, full, speech-language-filter, speech-transcription-check |
```

- [ ] **Step 6: Add parser snapshot coverage**

In `crates/voom-dsl/tests/parser_snapshots.rs`, add:

```rust
#[test]
fn snapshot_speech_language_filter_example() {
    let input = include_str!("../../../docs/examples/speech-language-filter.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}

#[test]
fn snapshot_speech_transcription_check_example() {
    let input = include_str!("../../../docs/examples/speech-transcription-check.voom");
    let ast = parse_policy(input).unwrap();
    assert_yaml_snapshot!(ast);
}
```

- [ ] **Step 7: Run validations**

Run:

```bash
cargo run -q -- policy validate docs/examples/speech-language-filter.voom
cargo run -q -- policy validate docs/examples/speech-transcription-check.voom
cargo run -q -- policy test docs/examples/tests/speech-language-filter.test.json
cargo run -q -- policy test docs/examples/tests/speech-transcription-check.test.json
cargo test -q -p voom-dsl parser_snapshots -- --nocapture
```

Expected: all commands pass. Snapshot tests may create or update snapshot files;
review them and include only relevant snapshots.

- [ ] **Step 8: Format and commit**

Run:

```bash
cargo fmt
```

Expected: no Rust formatting changes outside parser snapshot test additions.

Commit:

```bash
git add docs/examples/speech-language-filter.voom \
  docs/examples/speech-transcription-check.voom \
  docs/examples/README.md \
  docs/examples/tests/speech-dual-language.json \
  docs/examples/tests/speech-spanish-only.json \
  docs/examples/tests/speech-language-filter.test.json \
  docs/examples/tests/speech-transcription-check.test.json \
  crates/voom-dsl/tests/parser_snapshots.rs \
  crates/voom-dsl/tests/snapshots
git commit -m "docs: add speech corpus policy examples"
```

## Task 7: Run Adversarial Reviews And Fix Findings

**Files:**
- Create `docs/plans/issue-306-adversarial-review.md`
- Modify files from earlier tasks if review finds defects

- [ ] **Step 1: Create adversarial review document**

Create `docs/plans/issue-306-adversarial-review.md`:

```markdown
# Issue 306 Adversarial Review

## Plan Review

- Platform assumption check:
- CLI surface area check:
- Language coverage reliability check:
- Test evidence check:

## Code Review

- Command injection boundaries:
- Temporary file cleanup:
- Backend failure reporting:
- Fixture runtime:
- Manifest consistency:
- Linux support evidence:

## Documentation Review

- TTS setup accuracy:
- Transcription promise boundary:
- Manifest expectation clarity:
- Example policy accuracy:

## Outcomes

- Fixed before merge:
- Deferred follow-up issues:
```

- [ ] **Step 2: Fill plan review**

Read the final implementation plan and fill each Plan Review bullet with a
concrete pass/fail note. If a bullet fails, fix the plan or file a follow-up
issue before continuing.

- [ ] **Step 3: Fill code review**

Review the diff:

```bash
git diff main...HEAD -- scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Fill each Code Review bullet with evidence. Pay special attention to shell
argument handling: all TTS commands must use list argv, never shell strings.

- [ ] **Step 4: Fill documentation review**

Review:

```bash
git diff main...HEAD -- docs/test-corpus-generator.md \
  docs/functional-test-plan-tts-corpus.md docs/examples
```

Fill each Documentation Review bullet. Verify docs say manifest transcript text
is fixture intent, not guaranteed recognizer output.

- [ ] **Step 5: Fix any review findings**

For each valid finding, either:

- edit the relevant code/docs/tests and rerun verification, or
- create a GitHub issue with `gh issue create` and record the issue URL in
  `Deferred follow-up issues`.

Do not defer broken acceptance criteria for issue 306.

- [ ] **Step 6: Commit review artifact and fixes**

If fixes were needed, commit them with an appropriate conventional commit. Then
commit the review artifact:

```bash
git add docs/plans/issue-306-adversarial-review.md
git commit -m "docs: record issue 306 adversarial review"
```

## Task 8: Final Verification, PR, And Follow-Up Loop

**Files:**
- No planned file edits unless verification fails

- [ ] **Step 1: Run focused Python checks**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
uv run ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: all tests pass and Ruff reports no warnings.

- [ ] **Step 2: Run policy/example checks**

Run:

```bash
cargo run -q -- policy validate docs/examples/speech-language-filter.voom
cargo run -q -- policy validate docs/examples/speech-transcription-check.voom
cargo run -q -- policy test docs/examples/tests/speech-language-filter.test.json
cargo run -q -- policy test docs/examples/tests/speech-transcription-check.test.json
cargo test -q -p voom-dsl parser_snapshots
```

Expected: all commands pass.

- [ ] **Step 3: Run generator smoke commands**

Run:

```bash
rm -rf /tmp/voom-tts-none
scripts/generate-test-corpus /tmp/voom-tts-none --profile coverage \
  --only speech-english-aac --tts-backend none
jq '.summary, .skipped[0].reason' /tmp/voom-tts-none/manifest.json
```

Expected: one skipped TTS fixture with the no-backend reason.

If `espeak-ng` is installed, run:

```bash
rm -rf /tmp/voom-tts-espeak
scripts/generate-test-corpus /tmp/voom-tts-espeak --profile coverage \
  --only speech-english-aac,speech-dual-language --duration 3 \
  --tts-backend espeak-ng
jq '.summary, [.generated[].expect.speech_languages]' \
  /tmp/voom-tts-espeak/manifest.json
```

Expected: two generated files, speech language expectations present.

- [ ] **Step 4: Inspect git history**

Run:

```bash
git log --oneline main..HEAD
git status --short
```

Expected: small conventional commits and clean working tree.

- [ ] **Step 5: Push branch and open PR**

Run:

```bash
git push -u origin feat/issue-306-tts-test-corpus
gh pr create \
  --title "feat: add TTS fixtures to test corpus generator" \
  --body-file /tmp/issue-306-pr.md
```

Before `gh pr create`, write `/tmp/issue-306-pr.md` with:

```markdown
## Summary

- Adds TTS-backed speech fixtures to `scripts/generate-test-corpus`
- Supports Linux generation with `espeak-ng` and clean no-backend skips
- Documents speech corpus usage and adds example policies/tests

## Tests

- `uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py`
- `uv run ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py`
- `cargo run -q -- policy validate docs/examples/speech-language-filter.voom`
- `cargo run -q -- policy validate docs/examples/speech-transcription-check.voom`
- `cargo run -q -- policy test docs/examples/tests/speech-language-filter.test.json`
- `cargo run -q -- policy test docs/examples/tests/speech-transcription-check.test.json`
- `cargo test -q -p voom-dsl parser_snapshots`

Closes #306.
```

- [ ] **Step 6: Shepherd PR to completion**

Run:

```bash
gh pr checks --watch
gh pr view --json reviewDecision,mergeStateStatus,comments,reviews
```

Address CI failures or review comments with small conventional commits. Use the
receiving-code-review workflow before changing code in response to review.

- [ ] **Step 7: Merge PR**

When checks pass and the PR is approved:

```bash
gh pr merge --squash --delete-branch
```

Use the merge method preferred by repository maintainers if GitHub reports a
different required method.

- [ ] **Step 8: Handle deferred follow-up issues**

If `docs/plans/issue-306-adversarial-review.md` lists deferred issues, repeat
this full process for each newly created issue:

1. create a new feature branch from updated `main`
2. write or update a spec
3. write a plan
4. implement in small commits
5. run adversarial review
6. open and merge a PR

The original task is complete only when issue 306 and all generated follow-up
issues are merged via PR.
