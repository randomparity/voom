# Test Corpus Generator Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add profiled deterministic fixture coverage, procedural motion sources, deterministic corruption fixtures, and manifest output to `scripts/generate-test-corpus`.

**Architecture:** Keep the generator as one CLI script, but give fixtures explicit metadata (`profiles`, `covers`, `expect`) and add small pure helper functions for selection, manifest output, and corruption planning. Tests import the script by path and exercise those helpers without invoking FFmpeg; manual verification covers real FFmpeg output.

**Tech Stack:** Python 3 script, `argparse`, `json`, `pytest` for helper tests, FFmpeg/ffprobe for manual media verification.

---

## File Structure

- Modify `scripts/generate-test-corpus`: fixture metadata, profile filtering, procedural video sources, named corruption fixtures, run manifest writing, and CLI flags.
- Create `tests/scripts/test_generate_test_corpus.py`: pure Python tests for profile selection, manifest shape, deterministic corruption planning, and encoder skip reporting.
- Use existing `docs/superpowers/specs/2026-05-09-test-corpus-generator-design.md` as the design reference.

Do not split the generator into multiple modules in this implementation. The current repo has the script as a standalone executable, and the planned helpers are small enough to keep local.

## Task 1: Add Test Harness And Profile Selection

**Files:**
- Modify: `scripts/generate-test-corpus`
- Create: `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Write failing tests for profile selection**

Create `tests/scripts/test_generate_test_corpus.py`:

```python
"""Tests for scripts/generate-test-corpus pure helpers."""

import importlib.util
from importlib.machinery import SourceFileLoader
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "generate-test-corpus"


def load_generator():
    loader = SourceFileLoader("generate_test_corpus", str(SCRIPT_PATH))
    spec = importlib.util.spec_from_loader("generate_test_corpus", loader)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_select_specs_filters_by_profile_only_and_skip():
    generator = load_generator()
    specs = [
        {"stem": "a", "profiles": ["smoke", "coverage"]},
        {"stem": "b", "profiles": ["coverage"]},
        {"stem": "c", "profiles": ["stress"]},
    ]

    selected = generator.select_specs(
        specs,
        profile="coverage",
        only={"a", "b", "c"},
        skip={"b"},
    )

    assert [spec["stem"] for spec in selected] == ["a"]


def test_select_specs_all_includes_coverage_and_stress():
    generator = load_generator()
    specs = [
        {"stem": "coverage-case", "profiles": ["coverage"]},
        {"stem": "stress-case", "profiles": ["stress"]},
        {"stem": "smoke-case", "profiles": ["smoke", "coverage"]},
    ]

    selected = generator.select_specs(specs, profile="all", only=None, skip=set())

    assert [spec["stem"] for spec in selected] == [
        "coverage-case",
        "stress-case",
        "smoke-case",
    ]
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: fails with `AttributeError: module 'generate_test_corpus' has no attribute 'select_specs'`.

- [ ] **Step 3: Implement profile constants, metadata defaults, and selection**

Add constants after the metadata pools:

```python
SCHEMA_VERSION = 1
PROFILE_CHOICES = ("smoke", "coverage", "stress", "all")
ALL_PROFILE_MEMBERS = {"smoke", "coverage", "stress"}
```

Add helper functions before `build_manifest()`:

```python
def fixture_filename(spec):
    """Return the output filename for a fixture spec."""
    return f"{spec['stem']}.{spec['ext']}"


def profile_matches(spec, profile):
    """Return whether a fixture belongs to the selected profile."""
    profiles = set(spec.get("profiles", ["coverage"]))
    if profile == "all":
        return bool(profiles & ALL_PROFILE_MEMBERS)
    return profile in profiles


def select_specs(specs, profile, only, skip):
    """Filter fixture specs by profile, --only, and --skip."""
    selected = []
    for spec in specs:
        stem = spec["stem"]
        if not profile_matches(spec, profile):
            continue
        if only is not None and stem not in only:
            continue
        if stem in skip:
            continue
        selected.append(spec)
    return selected
```

Update every deterministic fixture in `build_manifest()` with explicit metadata. Use these minimum tags for existing fixtures:

```python
"profiles": ["smoke", "coverage"],
"covers": ["video.codec.h264", "audio.codec.aac"],
"expect": {"bad_file": False, "container": "mp4"},
```

Assign `smoke` to a small fast subset: `basic-h264-aac`, `loudness-quiet-dialogue`, `letterbox-h264`, `hevc-surround`, `vp9-opus`. Other existing deterministic fixtures get `["coverage"]` or `["coverage", "stress"]` for expensive 4K/optional encoders.

Update `main()` parser:

```python
parser.add_argument(
    "--profile",
    choices=PROFILE_CHOICES,
    default="coverage",
    help="Named deterministic fixture profile to generate (default: coverage)",
)
```

Replace the manual manifest filtering loop with:

```python
only = set(args.only.split(",")) if args.only else None
skip = set(args.skip.split(",")) if args.skip else set()
specs = select_specs(manifest, args.profile, only, skip)
```

- [ ] **Step 4: Run profile tests and verify they pass**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: both tests pass.

- [ ] **Step 5: Run dry-run smoke command**

Run:

```bash
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile smoke
```

Expected: prints only smoke-profile fixtures and exits 0.

- [ ] **Step 6: Commit Task 1**

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Add corpus generator profile selection"
```

## Task 2: Add Manifest Output And Result Tracking

**Files:**
- Modify: `scripts/generate-test-corpus`
- Modify: `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Write failing tests for manifest creation**

Append to `tests/scripts/test_generate_test_corpus.py`:

```python
def test_build_run_manifest_records_generated_skipped_failed_and_corruptions():
    generator = load_generator()

    manifest = generator.build_run_manifest(
        profile="coverage",
        duration=2,
        duration_range=(1, 5),
        count=3,
        generated=[
            {
                "filename": "basic-h264-aac.mp4",
                "size": 1234,
                "covers": ["video.codec.h264"],
                "expect": {"bad_file": False},
            }
        ],
        skipped=[{"filename": "av1-opus.mp4", "reason": "encoder 'libsvtav1' not available"}],
        failed=[{"filename": "bad.mkv", "reason": "ffmpeg failed"}],
        corruptions=[{"filename": "corrupt-truncated-tail.mkv", "type": "truncated_tail"}],
    )

    assert manifest["schema_version"] == 1
    assert manifest["settings"]["profile"] == "coverage"
    assert manifest["settings"]["duration"] == 2
    assert manifest["settings"]["duration_range"] == [1, 5]
    assert manifest["summary"] == {
        "generated": 1,
        "skipped": 1,
        "failed": 1,
        "corrupted": 1,
    }
    assert manifest["generated"][0]["covers"] == ["video.codec.h264"]
```

- [ ] **Step 2: Run test and verify it fails**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py::test_build_run_manifest_records_generated_skipped_failed_and_corruptions
```

Expected: fails with missing `build_run_manifest`.

- [ ] **Step 3: Implement manifest helper and write function**

Add helpers before `main()`:

```python
def build_run_manifest(
    profile,
    duration,
    duration_range,
    count,
    generated,
    skipped,
    failed,
    corruptions,
):
    """Build the JSON-serializable run manifest."""
    return {
        "schema_version": SCHEMA_VERSION,
        "settings": {
            "profile": profile,
            "duration": duration,
            "duration_range": list(duration_range),
            "count": count,
        },
        "summary": {
            "generated": len(generated),
            "skipped": len(skipped),
            "failed": len(failed),
            "corrupted": len(corruptions),
        },
        "generated": generated,
        "skipped": skipped,
        "failed": failed,
        "corruptions": corruptions,
    }


def write_run_manifest(dest, manifest):
    """Write manifest.json with stable formatting."""
    path = dest / "manifest.json"
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return path
```

In `main()`, initialize result lists before the generation loop:

```python
generated_entries = []
skipped_entries = []
failed_entries = []
```

When an encoder is unavailable, append:

```python
skipped_entries.append({
    "filename": filename,
    "stem": stem,
    "reason": f"encoder '{req}' not available",
    "covers": spec.get("covers", []),
    "expect": spec.get("expect", {}),
})
```

When command building or FFmpeg fails, append `failed_entries` with `filename`, `stem`, `reason`, `covers`, and `expect`.

When a file generates successfully, append:

```python
generated_entries.append({
    "filename": filename,
    "stem": stem,
    "size": size,
    "duration": file_duration,
    "profiles": spec.get("profiles", ["coverage"]),
    "covers": spec.get("covers", []),
    "expect": spec.get("expect", {}),
})
```

After random corruption and before the summary exits, build and write the manifest when not a dry run:

```python
corruption_entries = [
    {"filename": filename, "type": ctype, "description": desc}
    for filename, ctype, desc in corrupt_results
]
if not args.dry_run:
    run_manifest = build_run_manifest(
        profile=args.profile,
        duration=args.duration,
        duration_range=dur_range,
        count=args.count,
        generated=generated_entries,
        skipped=skipped_entries,
        failed=failed_entries,
        corruptions=corruption_entries,
    )
    manifest_path = write_run_manifest(dest, run_manifest)
    print(f"Manifest: {manifest_path}")
```

- [ ] **Step 4: Run manifest tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: all tests pass.

- [ ] **Step 5: Run smoke generation and inspect manifest**

Run:

```bash
trash /tmp/voom-corpus-smoke
scripts/generate-test-corpus /tmp/voom-corpus-smoke --profile smoke --duration 1
python3 -m json.tool /tmp/voom-corpus-smoke/manifest.json >/tmp/voom-corpus-smoke/manifest.pretty.json
```

Expected: generator exits 0, `manifest.json` exists, and `python3 -m json.tool` exits 0.

- [ ] **Step 6: Commit Task 2**

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Write corpus generator run manifest"
```

## Task 3: Add Procedural Mandelbrot Video Sources

**Files:**
- Modify: `scripts/generate-test-corpus`
- Modify: `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Write failing tests for video source construction**

Append:

```python
def test_build_video_input_uses_mandelbrot_source_and_black_bars():
    generator = load_generator()
    video = {
        "source": "mandelbrot_zoom",
        "size": "1920x1080",
        "active_size": "1920x816",
        "fps": 24,
    }

    source = generator.build_video_input(video, duration=2, specials={"black_bars"})

    assert source.startswith("mandelbrot=")
    assert "rate=24" in source
    assert "scale=1920:816" in source
    assert "pad=1920:1080:(ow-iw)/2:(oh-ih)/2:black" in source


def test_build_video_input_keeps_testsrc_for_smoke_fixture():
    generator = load_generator()
    video = {"source": "testsrc2", "size": "1280x720", "fps": 24}

    source = generator.build_video_input(video, duration=2, specials=set())

    assert source == "testsrc2=duration=2:size=1280x720:rate=24"
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py::test_build_video_input_uses_mandelbrot_source_and_black_bars
```

Expected: fails with missing `build_video_input`.

- [ ] **Step 3: Implement video source helper**

Add before `build_ffmpeg_cmd()`:

```python
def build_video_input(video, duration, specials):
    """Build a lavfi video input expression for a fixture."""
    source = video.get("source", "testsrc2")
    active_size = video.get("active_size", video["size"])
    active_w, active_h = active_size.split("x", 1)
    fps = video["fps"]

    if source == "mandelbrot_zoom":
        video_input = (
            f"mandelbrot=size={active_size}:rate={fps}:"
            "start_x=-0.7436438870371587:start_y=0.131825904205312:"
            "start_scale=3:end_scale=0.0008"
            f",trim=duration={duration},setpts=PTS-STARTPTS"
        )
        if active_size != video["size"]:
            video_input += f",scale={active_w}:{active_h}"
    elif source == "testsrc2":
        video_input = f"testsrc2=duration={duration}:size={active_size}:rate={fps}"
    else:
        raise ValueError(f"unsupported video source '{source}'")

    if "vfr" in specials:
        video_input += ",setpts='PTS+0.003*sin(N*0.15)/TB'"
    if "black_bars" in specials:
        output_size = video["size"].replace("x", ":")
        video_input += f",pad={output_size}:(ow-iw)/2:(oh-ih)/2:black"

    return video_input
```

Replace the duplicated video input construction in `build_ffmpeg_cmd()` with:

```python
video_input = build_video_input(video, duration, specials)
cmd += ["-f", "lavfi", "-i", video_input]
```

Update coverage fixtures that need quality-sensitive content with `"source": "mandelbrot_zoom"`:

- `letterbox-h264`
- `pillarbox-h264`
- `4k-hevc-hdr10`
- `vfr-h264`
- new fixtures added in Task 5

Leave the smallest smoke-only fixture on `testsrc2`.

- [ ] **Step 4: Run tests and dry-run coverage**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile coverage --only letterbox-h264
```

Expected: tests pass, dry-run command contains `mandelbrot=` for `letterbox-h264`.

- [ ] **Step 5: Commit Task 3**

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Add procedural corpus video sources"
```

## Task 4: Add Deterministic Corrupt Fixtures

**Files:**
- Modify: `scripts/generate-test-corpus`
- Modify: `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Write failing tests for deterministic corruption planning**

Append:

```python
def test_collect_deterministic_corruptions_selects_profile_members():
    generator = load_generator()
    specs = [
        {
            "stem": "corrupt-truncated-tail",
            "ext": "mkv",
            "profiles": ["coverage"],
            "corruption": {
                "source_stem": "basic-h264-aac",
                "source_ext": "mp4",
                "type": "truncated_tail",
            },
        },
        {
            "stem": "corrupt-stress",
            "ext": "mkv",
            "profiles": ["stress"],
            "corruption": {
                "source_stem": "basic-h264-aac",
                "source_ext": "mp4",
                "type": "mid_stream",
            },
        },
    ]

    selected = generator.collect_deterministic_corruptions(specs, profile="coverage")

    assert selected == [specs[0]]
```

- [ ] **Step 2: Run test and verify failure**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py::test_collect_deterministic_corruptions_selects_profile_members
```

Expected: fails with missing `collect_deterministic_corruptions`.

- [ ] **Step 3: Implement deterministic corrupt specs and transforms**

Add corruption specs to `build_manifest()` with `corruption` metadata and no normal `video` generation. Example:

```python
{
    "stem": "corrupt-truncated-tail",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["bad_file.truncated_tail"],
    "expect": {"bad_file": True, "corruption": "truncated_tail"},
    "corruption": {
        "source_stem": "basic-h264-aac",
        "source_ext": "mp4",
        "type": "truncated_tail",
    },
},
```

Add deterministic corrupt fixtures:

- `corrupt-truncated-tail`
- `corrupt-zero-length`
- `corrupt-header-damage`
- `corrupt-midstream-bitrot`
- `corrupt-wrong-extension`
- `corrupt-container-metadata`

Add helpers:

```python
def collect_deterministic_corruptions(specs, profile):
    """Return corruption fixture specs selected by profile."""
    return [
        spec for spec in specs
        if "corruption" in spec and profile_matches(spec, profile)
    ]


def materialize_corrupt_fixture(dest, spec, rng):
    """Create a deterministic corrupt fixture from an existing valid source."""
    corruption = spec["corruption"]
    source = dest / f"{corruption['source_stem']}.{corruption['source_ext']}"
    target = dest / fixture_filename(spec)
    if not source.exists():
        return {
            "filename": fixture_filename(spec),
            "type": corruption["type"],
            "description": f"skipped; source missing: {source.name}",
            "skipped": True,
        }

    target.write_bytes(source.read_bytes())
    ctype = corruption["type"]
    apply_type = "truncated" if ctype == "truncated_tail" else ctype
    if ctype == "container_metadata":
        apply_type = "header_damage"
    desc = apply_corruption(target, apply_type, rng)
    return {
        "filename": fixture_filename(spec),
        "type": ctype,
        "description": desc,
        "skipped": False,
    }
```

Update selection so normal generation skips specs containing `corruption`:

```python
deterministic_corrupt_specs = collect_deterministic_corruptions(specs, args.profile)
specs = [spec for spec in specs if "corruption" not in spec]
```

After valid files are generated and before random corruption, materialize deterministic corrupt fixtures:

```python
deterministic_corruptions = []
for spec in deterministic_corrupt_specs:
    result = materialize_corrupt_fixture(dest, spec, random.Random(args.seed + 7000))
    deterministic_corruptions.append(result)
    print(f"  CORRUPTED {result['filename']} — {result['type']}: {result['description']}")
```

Include deterministic corruptions in manifest corruption entries.

- [ ] **Step 4: Run tests and dry-run**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile coverage --only corrupt-truncated-tail
```

Expected: tests pass; dry-run lists the selected corruption fixture without trying to build an FFmpeg command for it.

- [ ] **Step 5: Run small real corruption generation**

Run:

```bash
trash /tmp/voom-corpus-corrupt
scripts/generate-test-corpus /tmp/voom-corpus-corrupt --profile coverage --duration 1 --only basic-h264-aac,corrupt-truncated-tail
python3 -m json.tool /tmp/voom-corpus-corrupt/manifest.json >/tmp/voom-corpus-corrupt/manifest.pretty.json
```

Expected: valid source file and `corrupt-truncated-tail.mkv` exist; manifest lists one deterministic corruption.

- [ ] **Step 6: Commit Task 4**

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Add deterministic corrupt corpus fixtures"
```

## Task 5: Expand HDR, Crop, And Audio Coverage

**Files:**
- Modify: `scripts/generate-test-corpus`

- [ ] **Step 1: Add HDR/color fixture specs**

Add deterministic fixtures to `build_manifest()`:

```python
{
    "stem": "hevc-hlg-10bit",
    "ext": "mkv",
    "profiles": ["coverage", "stress"],
    "covers": ["video.content.mandelbrot_zoom", "video.hdr.hlg", "video.codec.hevc"],
    "expect": {"bad_file": False, "video_codec": "hevc", "is_hdr": True, "hdr_format": "hlg"},
    "video": {"source": "mandelbrot_zoom", "codec": "libx265", "size": "1920x1080", "fps": 24},
    "audio": [{"codec": "aac", "channels": 2, "lang": "eng"}],
    "subs": [],
    "special": ["hlg"],
},
{
    "stem": "hevc-sdr-10bit",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["video.bit_depth.10", "video.hdr.false_positive_guard", "video.codec.hevc"],
    "expect": {"bad_file": False, "video_codec": "hevc", "is_hdr": False, "pixel_format": "yuv420p10le"},
    "video": {"source": "mandelbrot_zoom", "codec": "libx265", "size": "1920x1080", "fps": 24},
    "audio": [{"codec": "aac", "channels": 2, "lang": "eng"}],
    "subs": [],
    "special": ["sdr_10bit"],
},
```

Update video codec handling so HDR10 and HLG choose their own HEVC metadata
bitstream filter:

```python
hevc_metadata_bsf = None

if "hlg" in specials:
    cmd += ["-pix_fmt", "yuv420p10le"]
    if vcodec == "libx265":
        cmd += [
            "-x265-params",
            "repeat-headers=1:colorprim=bt2020:transfer=arib-std-b67:"
            "colormatrix=bt2020nc",
        ]
        hevc_metadata_bsf = (
            "hevc_metadata=colour_primaries=9:"
            "transfer_characteristics=18:matrix_coefficients=9"
        )

if "sdr_10bit" in specials:
    cmd += ["-pix_fmt", "yuv420p10le"]
```

Update the existing HDR10 branch to set:

```python
hevc_metadata_bsf = (
    "hevc_metadata=colour_primaries=9:"
    "transfer_characteristics=16:matrix_coefficients=9"
)
```

Replace the final hard-coded HEVC metadata block with:

```python
if hevc_metadata_bsf is not None:
    cmd += ["-bsf:v", hevc_metadata_bsf]
```

- [ ] **Step 2: Add crop fixture specs**

Add:

```python
{
    "stem": "windowbox-h264",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["video.crop.windowbox", "video.content.mandelbrot_zoom"],
    "expect": {"bad_file": False, "crop": {"left": 240, "top": 120, "right": 240, "bottom": 120}},
    "video": {
        "source": "mandelbrot_zoom",
        "codec": "libx264",
        "size": "1920x1080",
        "active_size": "1440x840",
        "fps": 24,
    },
    "audio": [{"codec": "aac", "channels": 2, "lang": "eng"}],
    "subs": [],
    "special": ["black_bars"],
},
{
    "stem": "dark-edge-no-crop",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["video.crop.false_positive_guard"],
    "expect": {"bad_file": False, "crop": None},
    "video": {"source": "mandelbrot_zoom", "codec": "libx264", "size": "1920x1080", "fps": 24},
    "audio": [{"codec": "aac", "channels": 2, "lang": "eng"}],
    "subs": [],
    "special": ["dark_edges"],
},
```

For `dark_edges`, append a mild vignette instead of black bars in `build_video_input()`:

```python
if "dark_edges" in specials:
    video_input += ",vignette=PI/5"
```

- [ ] **Step 3: Add audio normalization fixture specs**

Add:

```python
{
    "stem": "loudness-normalized-target",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["audio.normalize.already_normalized", "audio.loudness.target"],
    "expect": {"bad_file": False, "audio_lufs": -23.0},
    "video": {"source": "mandelbrot_zoom", "codec": "libx264", "size": "1280x720", "fps": 24},
    "audio": [{"codec": "aac", "channels": 2, "volume": 0.65, "lang": "eng"}],
    "subs": [],
    "special": ["audio_loudnorm_target"],
},
{
    "stem": "loudness-dynamic-bursts",
    "ext": "mkv",
    "profiles": ["coverage"],
    "covers": ["audio.normalize.dynamic_range", "audio.loudness.bursts"],
    "expect": {"bad_file": False, "audio_tracks": 1},
    "video": {"source": "mandelbrot_zoom", "codec": "libx264", "size": "1280x720", "fps": 24},
    "audio": [{"codec": "aac", "channels": 2, "lang": "eng", "source": "dynamic_bursts"}],
    "subs": [],
    "special": ["loudness"],
},
{
    "stem": "surround-7-1-flac",
    "ext": "mkv",
    "profiles": ["coverage", "stress"],
    "covers": ["audio.channels.7_1", "audio.codec.flac"],
    "expect": {"bad_file": False, "audio_tracks": 1},
    "video": {"source": "mandelbrot_zoom", "codec": "libx264", "size": "1280x720", "fps": 24},
    "audio": [{"codec": "flac", "channels": 8, "lang": "eng"}],
    "subs": [],
    "special": [],
},
```

Add audio source helper if needed:

```python
def build_audio_input(audio, index, duration):
    """Build a lavfi audio input expression for a fixture."""
    source = audio.get("source", "sine")
    if source == "dynamic_bursts":
        return (
            f"aevalsrc='if(between(mod(t,1),0,0.15),0.9*sin(2*PI*880*t),"
            f"0.04*sin(2*PI*220*t))':duration={duration}:sample_rate=48000"
        )
    audio_filter = f"sine=frequency={440 + index * 110}:duration={duration}:sample_rate=48000"
    if "volume" in audio:
        audio_filter += f",volume={audio['volume']}"
    return audio_filter
```

Use `build_audio_input(audio, i, duration)` in `build_ffmpeg_cmd()`.

For `audio_loudnorm_target`, add an audio filter on stream 0:

```python
if "audio_loudnorm_target" in specials:
    cmd += ["-af:a:0", "loudnorm=I=-23:TP=-2:LRA=11"]
```

- [ ] **Step 4: Run dry-run checks for new fixtures**

Run:

```bash
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile coverage --only hevc-hlg-10bit,hevc-sdr-10bit,windowbox-h264,dark-edge-no-crop,loudness-normalized-target,loudness-dynamic-bursts
```

Expected: exits 0 and prints FFmpeg commands containing the expected source/filter markers.

- [ ] **Step 5: Run focused real generation**

Run:

```bash
trash /tmp/voom-corpus-expanded
scripts/generate-test-corpus /tmp/voom-corpus-expanded --profile coverage --duration 1 --only windowbox-h264,loudness-dynamic-bursts
python3 -m json.tool /tmp/voom-corpus-expanded/manifest.json >/tmp/voom-corpus-expanded/manifest.pretty.json
```

Expected: exits 0 and writes two generated fixture entries.

- [ ] **Step 6: Commit Task 5**

```bash
git add scripts/generate-test-corpus
git commit -m "Expand corpus media coverage fixtures"
```

## Task 6: Final Verification And Cleanup

**Files:**
- Modify only if verification reveals issues: `scripts/generate-test-corpus`, `tests/scripts/test_generate_test_corpus.py`

- [ ] **Step 1: Run Python helper tests**

Run:

```bash
uv run --with pytest pytest -q tests/scripts/test_generate_test_corpus.py
```

Expected: all tests pass.

- [ ] **Step 2: Run dry-run profiles**

Run:

```bash
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile smoke
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile coverage
scripts/generate-test-corpus /tmp/voom-corpus-dry --dry-run --profile stress
```

Expected: all commands exit 0.

- [ ] **Step 3: Run real smoke generation**

Run:

```bash
trash /tmp/voom-corpus-smoke
scripts/generate-test-corpus /tmp/voom-corpus-smoke --profile smoke --duration 1
python3 -m json.tool /tmp/voom-corpus-smoke/manifest.json >/tmp/voom-corpus-smoke/manifest.pretty.json
```

Expected: exits 0 and manifest JSON validates.

- [ ] **Step 4: Review generated coverage tags**

Run:

```bash
python3 - <<'PY'
import json
from pathlib import Path

manifest = json.loads(Path("/tmp/voom-corpus-smoke/manifest.json").read_text())
tags = sorted({tag for item in manifest["generated"] for tag in item["covers"]})
print("\n".join(tags))
PY
```

Expected: prints non-empty coverage tags.

- [ ] **Step 5: Run formatter/lint checks applicable to touched Python**

Run:

```bash
ruff format scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
ruff check scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: both commands exit 0. If `ruff` is unavailable in the environment, record that in the final implementation summary and rely on the passing pytest and dry-run checks.

- [ ] **Step 6: Review diff for scope and complexity**

Run:

```bash
git diff --stat
git diff -- scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
```

Expected: changes are limited to the generator and its tests; no VOOM runtime code changed.

- [ ] **Step 7: Commit final fixes if any**

If Step 5 or Step 6 required edits:

```bash
git add scripts/generate-test-corpus tests/scripts/test_generate_test_corpus.py
git commit -m "Polish corpus generator coverage"
```

If no edits were required, do not create an empty commit.
