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


def test_build_manifest_smoke_profile_is_expected_fast_subset():
    generator = load_generator()

    selected = generator.select_specs(
        generator.build_manifest(),
        profile="smoke",
        only=None,
        skip=set(),
    )

    assert [spec["stem"] for spec in selected] == [
        "basic-h264-aac",
        "loudness-quiet-dialogue",
        "letterbox-h264",
        "hevc-surround",
        "vp9-opus",
    ]


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
                "stem": "basic-h264-aac",
                "size": 1234,
                "duration": 2,
                "profiles": ["coverage"],
                "covers": ["video.codec.h264"],
                "expect": {"bad_file": False},
            }
        ],
        skipped=[
            {
                "filename": "av1-opus.mp4",
                "stem": "av1-opus",
                "reason": "encoder 'libsvtav1' not available",
                "covers": ["video.codec.av1"],
                "expect": {"bad_file": False},
            }
        ],
        failed=[
            {
                "filename": "bad.mkv",
                "stem": "bad",
                "reason": "ffmpeg failed",
                "covers": ["audio.codec.truehd"],
                "expect": {"bad_file": False},
            }
        ],
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
    assert manifest["skipped"][0]["covers"] == ["video.codec.av1"]
    assert manifest["failed"][0]["covers"] == ["audio.codec.truehd"]


def test_write_run_manifest_uses_stable_json_format(tmp_path):
    generator = load_generator()
    manifest = {"z": 1, "a": {"b": 2}}

    path = generator.write_run_manifest(tmp_path, manifest)

    assert path == tmp_path / "manifest.json"
    assert path.read_text() == '{\n  "a": {\n    "b": 2\n  },\n  "z": 1\n}\n'


def test_select_and_corrupt_reports_final_filenames_and_sizes(tmp_path):
    generator = load_generator()
    specs = [
        {"stem": "zero-case", "ext": "mkv"},
        {"stem": "truncated-case", "ext": "mkv"},
        {"stem": "rename-case", "ext": "mp4"},
    ]

    for spec in specs:
        path = tmp_path / f"{spec['stem']}.{spec['ext']}"
        path.write_bytes(bytes(range(256)) * 8)

    corruptions = []
    original_types = generator.CORRUPTION_TYPES
    try:
        for index, corruption_type in enumerate(
            ["zero_length", "truncated", "wrong_extension"]
        ):
            generator.CORRUPTION_TYPES = [corruption_type]
            corruptions.extend(
                generator.select_and_corrupt(
                    tmp_path,
                    [specs[index]],
                    corrupt_count=1,
                    seed=12,
                )
            )
    finally:
        generator.CORRUPTION_TYPES = original_types

    assert {result["type"] for result in corruptions} == {
        "zero_length",
        "truncated",
        "wrong_extension",
    }

    for result in corruptions:
        final_path = tmp_path / result["filename"]
        assert final_path.exists()
        assert result["size"] == final_path.stat().st_size

    renamed = next(result for result in corruptions if result["type"] == "wrong_extension")
    assert renamed["original_filename"] == "rename-case.mp4"
    assert renamed["filename"] == "rename-case.mkv"
    assert not (tmp_path / renamed["original_filename"]).exists()

    zeroed = next(result for result in corruptions if result["type"] == "zero_length")
    assert zeroed["size"] == 0

    generated_entries = [
        {
            "filename": f"{spec['stem']}.{spec['ext']}",
            "size": 2048,
        }
        for spec in specs
    ]

    generator.update_generated_entries_for_corruptions(generated_entries, corruptions)

    entries_by_name = {entry["filename"]: entry for entry in generated_entries}
    for result in corruptions:
        entry = entries_by_name[result["filename"]]
        assert entry["size"] == result["size"]
