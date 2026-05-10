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
