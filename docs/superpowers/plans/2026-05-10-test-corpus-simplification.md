# Test Corpus Generator Simplification Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> or superpowers:subagent-driven-development to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the simplification review recommendations for
`scripts/generate-test-corpus` without changing generated fixture behavior,
manifest shape, or CLI behavior.

**Architecture:** Keep the corpus generator as a single executable script. Replace
duplicated manifest and corruption planning logic with small local helpers, convert
hand-written state containers to dataclasses, reduce avoidable FFmpeg process work,
and simplify tests that repeatedly load the script module.

**Tech Stack:** Python 3 script, standard library only, pytest helper tests, Ruff
format/check.

---

## File Structure

- Modify `scripts/generate-test-corpus`: helper consolidation, dataclasses,
  FFmpeg availability check, and optional in-place corruption I/O.
- Modify `tests/scripts/test_generate_test_corpus.py`: generator fixture and
  monkeypatch-based corruption type overrides.

Do not split the script into modules and do not add dependencies. This is a
simplification pass over the current implementation, not a feature pass.

## Task 1: Consolidate Issue Manifest Entries

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] Add a helper that captures the shared skipped/failed entry shape:

```python
def build_issue_entry(spec, filename, reason):
    """Return a skipped/failed manifest entry."""
    return {
        "filename": filename,
        "stem": spec["stem"],
        "reason": reason,
        "covers": spec.get("covers", []),
        "expect": spec.get("expect", {}),
    }
```

- [ ] Replace calls to `build_failed_generation_entry()` and
  `build_skipped_generation_entry()` with `build_issue_entry()`.
- [ ] Remove `build_failed_generation_entry()` and
  `build_skipped_generation_entry()`.
- [ ] Leave `build_generation_entry()` separate because it has generated-file
  fields (`size`, `duration`, `profiles`) that skipped/failed entries do not.
- [ ] Either keep `build_skipped_corruption_entry()` if that name helps call-site
  clarity, or make it a tiny wrapper around `build_issue_entry()`:

```python
def build_skipped_corruption_entry(spec, result):
    """Return a skipped manifest entry for an unmaterialized corruption fixture."""
    return build_issue_entry(spec, result["filename"], result["description"])
```

- [ ] Run the focused tests:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: all tests pass with unchanged skipped/failed manifest entries.

## Task 2: Centralize Corruption Filename Planning

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] Add a pure helper near the corruption helpers:

```python
def corruption_final_path(file_path, corruption_type):
    """Return the path a corruption operation will leave behind."""
    if corruption_type != "wrong_extension":
        return file_path
    if file_path.suffix == ".mkv":
        return file_path.with_suffix(".mp4")
    return file_path.with_suffix(".mkv")
```

- [ ] Update `apply_corruption()` to use `corruption_final_path()` for
  `wrong_extension` instead of duplicating extension selection.
- [ ] Update `materialize_deterministic_corruptions()` dry-run final filename
  prediction to call `corruption_final_path(Path(filename), applied_type)`, where
  `applied_type` is the actual type passed to `apply_corruption()`.
- [ ] Add or adjust tests to cover `.mp4 -> .mkv`, `.mkv -> .mp4`, and unchanged
  paths for non-rename corruption types.
- [ ] Run the focused tests:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: deterministic dry-runs and materialized wrong-extension corruptions
still report the same final filenames.

## Task 3: Convert State Containers To Dataclasses

**Files:**
- Modify `scripts/generate-test-corpus`

- [ ] Add the import:

```python
from dataclasses import dataclass, field
```

- [ ] Replace `PreparedSpecs`, `GenerationOptions`, `GenerationResult`, and
  `CorruptionResult` hand-written classes with dataclasses:

```python
@dataclass
class PreparedSpecs:
    """Fixture specs selected for a run."""

    specs: list
    deterministic_corrupt_specs: list
    random_specs: list


@dataclass
class GenerationOptions:
    """Options needed while materializing normal fixtures."""

    dest: Path
    duration: int
    seed: int
    dry_run: bool
    verbose: bool
    available: set
    total: int


@dataclass
class GenerationResult:
    """Mutable counts and manifest entries accumulated during a run."""

    ok_count: int = 0
    skip_count: int = 0
    fail_count: int = 0
    total_size: int = 0
    generated_entries: list = field(default_factory=list)
    skipped_entries: list = field(default_factory=list)
    failed_entries: list = field(default_factory=list)


@dataclass
class CorruptionResult:
    """Counts and corruption entries accumulated during corruption steps."""

    ok_count: int = 0
    skip_count: int = 0
    skipped_entries: list = field(default_factory=list)
    corrupt_results: list = field(default_factory=list)
```

- [ ] Prefer broad built-in annotations only if the repo tooling accepts them;
  avoid adding complex type aliases in this pass.
- [ ] Run tests and Ruff:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
uv run --with ruff ruff format --check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run --with ruff ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: behavior unchanged; no mutable default warnings or formatting changes.

## Task 4: Simplify FFmpeg Availability Check

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] Replace the extra `ffmpeg -version` subprocess in
  `ensure_ffmpeg_available()` with `shutil.which("ffmpeg")`.
- [ ] Keep the existing error message and exit behavior:

```python
def ensure_ffmpeg_available():
    """Exit with a clear message if ffmpeg is unavailable."""
    if shutil.which("ffmpeg") is None:
        print("Error: ffmpeg not found in PATH", file=sys.stderr)
        sys.exit(1)
```

- [ ] Leave `probe_encoders()` responsible for the one real FFmpeg probe.
- [ ] Add a small test that monkeypatches `shutil.which` to return `None` and
  asserts `SystemExit(1)` plus the stderr message.
- [ ] Add a small test that monkeypatches `shutil.which` to return a path and
  asserts the helper returns normally.
- [ ] Run the focused tests:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: availability behavior remains the same, while startup avoids one
redundant FFmpeg subprocess.

## Task 5: Simplify Test Module Loading

**Files:**
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] Convert repeated `load_generator()` calls to a pytest fixture:

```python
import pytest


@pytest.fixture
def generator():
    return load_generator()
```

- [ ] Update tests to accept `generator` as an argument instead of assigning
  `generator = load_generator()` inside each test.
- [ ] Replace manual `CORRUPTION_TYPES` save/restore logic with `monkeypatch`:

```python
monkeypatch.setattr(generator, "CORRUPTION_TYPES", [corruption_type])
```

- [ ] Keep tests behavior-focused. Do not assert implementation details that
  would make future refactors harder.
- [ ] Run the focused tests:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: same coverage with less repeated setup code.

## Task 6: Optional In-Place Corruption I/O Simplification

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

Only do this task if it stays small and all existing corruption tests continue to
pass. The behavior must remain deterministic for a fixed seed.

- [ ] Update truncation to use `Path.open("r+b")` and `truncate(new_size)` rather
  than reading and rewriting the prefix.
- [ ] Update zero-length corruption to use `truncate(0)`.
- [ ] Update header corruption to open `r+b`, generate the same number of random
  bytes, seek to 0, and write only the damaged header block.
- [ ] Update mid-stream corruption to open `r+b`, generate the same number of
  random bytes, seek to the selected offset, and write only that block.
- [ ] Keep RNG draw counts the same as today:
  - `truncated`: one `randint(10, 80)`
  - `header_damage`: one `randint(64, 512)` plus one byte draw per damaged byte
  - `mid_stream`: one block-size draw, one offset draw, plus one byte draw per
    overwritten byte
- [ ] If any fixture output or tests become unstable, skip this task and record
  it as deferred. The first five tasks address the low-risk recommendations.
- [ ] Run the focused tests:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: corruptions keep the same final names, sizes, and deterministic
metadata; I/O no longer rewrites whole files for partial corruptions.

## Task 7: Final Verification And Commit

**Files:**
- Modify `scripts/generate-test-corpus`
- Modify `tests/scripts/test_generate_test_corpus.py`

- [ ] Run formatting, linting, and focused tests:

```bash
uv run --with ruff ruff format scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run --with ruff ruff format --check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run --with ruff ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

- [ ] Run a no-media-output dry-run smoke check:

```bash
scripts/generate-test-corpus /tmp/voom-corpus-simplify-dry --dry-run --profile smoke
```

Expected: exits 0 and lists the same smoke profile commands as before.

- [ ] If Task 6 was implemented, run a small real corruption smoke check:

```bash
tmpdir="$(mktemp -d)"
scripts/generate-test-corpus "$tmpdir" --profile coverage --only basic-h264-aac --duration 1
scripts/generate-test-corpus "$tmpdir" --profile coverage --only corrupt-wrong-extension --duration 1
```

Expected: `corrupt-wrong-extension` materializes with the same final name and
manifest metadata as before.

- [ ] Review the diff for behavior changes:

```bash
git diff -- scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Confirm the diff is limited to simplification and tests. Do not add new corpus
features in this branch.

- [ ] Commit the simplification:

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Simplify corpus generator helpers"
```

## Risk Notes

- Corruption filename planning must use the applied corruption type, not the
  external deterministic corruption label. For example, `truncated_tail` maps to
  `truncated`, while `wrong_extension` stays `wrong_extension`.
- Dry-run output is user-facing enough to preserve. Avoid changing wording except
  where centralizing logic requires the same final filename detail.
- Dataclasses should not introduce shared mutable defaults. Use
  `field(default_factory=list)` for mutable result lists.
- Do not add static typing that forces a wider annotation cleanup. The target is
  readable simplification, not a typing migration.
