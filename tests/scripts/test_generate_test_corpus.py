"""Tests for scripts/generate-test-corpus pure helpers."""

import importlib.util
import random
from importlib.machinery import SourceFileLoader
from pathlib import Path

import pytest


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "scripts" / "generate-test-corpus"


def load_generator():
    loader = SourceFileLoader("generate_test_corpus", str(SCRIPT_PATH))
    spec = importlib.util.spec_from_loader("generate_test_corpus", loader)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def generator():
    return load_generator()


def test_select_specs_filters_by_profile_only_and_skip(generator):
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


def test_select_specs_all_includes_coverage_and_stress(generator):
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


def test_build_manifest_smoke_profile_is_expected_fast_subset(generator):
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


def test_build_run_manifest_records_generated_skipped_failed_and_corruptions(generator):
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
        corruptions=[
            {"filename": "corrupt-truncated-tail.mp4", "type": "truncated_tail"}
        ],
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


def test_write_run_manifest_uses_stable_json_format(generator, tmp_path):
    manifest = {"z": 1, "a": {"b": 2}}

    path = generator.write_run_manifest(tmp_path, manifest)

    assert path == tmp_path / "manifest.json"
    assert path.read_text() == '{\n  "a": {\n    "b": 2\n  },\n  "z": 1\n}\n'


def test_corruption_final_path_renames_mp4_to_mkv(generator):
    path = Path("fixture.mp4")

    final_path = generator.corruption_final_path(path, "wrong_extension")

    assert final_path == Path("fixture.mkv")


def test_corruption_final_path_renames_mkv_to_mp4(generator):
    path = Path("fixture.mkv")

    final_path = generator.corruption_final_path(path, "wrong_extension")

    assert final_path == Path("fixture.mp4")


def test_corruption_final_path_keeps_non_rename_corruptions(generator):
    path = Path("fixture.mp4")

    final_path = generator.corruption_final_path(path, "truncated")

    assert final_path == path


def test_select_and_corrupt_reports_final_filenames_and_sizes(
    generator, monkeypatch, tmp_path
):
    specs = [
        {"stem": "zero-case", "ext": "mkv"},
        {"stem": "truncated-case", "ext": "mkv"},
        {"stem": "rename-case", "ext": "mp4"},
    ]

    for spec in specs:
        path = tmp_path / f"{spec['stem']}.{spec['ext']}"
        path.write_bytes(bytes(range(256)) * 8)

    corruptions = []
    for index, corruption_type in enumerate(
        ["zero_length", "truncated", "wrong_extension"]
    ):
        monkeypatch.setattr(generator, "CORRUPTION_TYPES", [corruption_type])
        corruptions.extend(
            generator.select_and_corrupt(
                tmp_path,
                [specs[index]],
                corrupt_count=1,
                seed=12,
            )
        )

    assert {result["type"] for result in corruptions} == {
        "zero_length",
        "truncated",
        "wrong_extension",
    }

    for result in corruptions:
        final_path = tmp_path / result["filename"]
        assert final_path.exists()
        assert result["size"] == final_path.stat().st_size

    renamed = next(
        result for result in corruptions if result["type"] == "wrong_extension"
    )
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


def test_collect_deterministic_corruptions_selects_profile_members(generator):
    specs = [
        {
            "stem": "corrupt-truncated-tail",
            "ext": "mp4",
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


def test_build_manifest_includes_required_corrupt_fixtures(generator):
    corrupt_names = {
        spec["stem"] for spec in generator.build_manifest() if "corruption" in spec
    }

    assert corrupt_names == {
        "corrupt-truncated-tail",
        "corrupt-zero-length",
        "corrupt-header-damage",
        "corrupt-midstream-bitrot",
        "corrupt-wrong-extension",
        "corrupt-container-metadata",
    }


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


def test_materialize_corrupt_fixture_creates_final_file(generator, tmp_path):
    source = tmp_path / "basic-h264-aac.mp4"
    source.write_bytes(bytes(range(256)) * 16)
    spec = {
        "stem": "corrupt-truncated-tail",
        "ext": "mp4",
        "corruption": {
            "source_stem": "basic-h264-aac",
            "source_ext": "mp4",
            "type": "truncated_tail",
        },
    }

    result = generator.materialize_corrupt_fixture(tmp_path, spec, random.Random(7))

    final_path = tmp_path / result["filename"]
    assert result["original_filename"] == "corrupt-truncated-tail.mp4"
    assert result["filename"] == "corrupt-truncated-tail.mp4"
    assert result["type"] == "truncated_tail"
    assert result["size"] == final_path.stat().st_size
    assert result["skipped"] is False
    assert final_path.exists()


def test_deterministic_corruption_manifest_entry_retains_fixture_metadata(
    generator, tmp_path
):
    source = tmp_path / "basic-h264-aac.mp4"
    source.write_bytes(bytes(range(256)) * 16)
    spec = {
        "stem": "corrupt-truncated-tail",
        "ext": "mp4",
        "profiles": ["coverage"],
        "covers": ["bad_file.truncated_tail"],
        "expect": {"bad_file": True, "corruption": "truncated_tail"},
        "corruption": {
            "source_stem": "basic-h264-aac",
            "source_ext": "mp4",
            "type": "truncated_tail",
        },
    }

    result = generator.materialize_corrupt_fixture(tmp_path, spec, random.Random(7))
    entry = generator.build_corruption_manifest_entries([result])[0]

    final_path = tmp_path / "corrupt-truncated-tail.mp4"
    assert entry["stem"] == "corrupt-truncated-tail"
    assert entry["filename"] == "corrupt-truncated-tail.mp4"
    assert entry["source_filename"] == "basic-h264-aac.mp4"
    assert entry["covers"] == ["bad_file.truncated_tail"]
    assert entry["expect"]["bad_file"] is True
    assert entry["profiles"] == ["coverage"]
    assert entry["size"] == final_path.stat().st_size


def test_materialize_corrupt_fixture_reports_missing_source(generator, tmp_path):
    spec = {
        "stem": "corrupt-truncated-tail",
        "ext": "mp4",
        "corruption": {
            "source_stem": "basic-h264-aac",
            "source_ext": "mp4",
            "type": "truncated_tail",
        },
    }

    result = generator.materialize_corrupt_fixture(tmp_path, spec, random.Random(7))

    assert result == {
        "original_filename": "corrupt-truncated-tail.mp4",
        "filename": "corrupt-truncated-tail.mp4",
        "source_filename": "basic-h264-aac.mp4",
        "type": "truncated_tail",
        "description": "skipped (source missing: basic-h264-aac.mp4)",
        "size": 0,
        "skipped": True,
    }
    assert not (tmp_path / "corrupt-truncated-tail.mp4").exists()


def test_materialize_corrupt_fixture_reports_wrong_extension_final_name(
    generator, tmp_path
):
    source = tmp_path / "basic-h264-aac.mp4"
    source.write_bytes(bytes(range(256)) * 16)
    spec = {
        "stem": "corrupt-wrong-extension",
        "ext": "mp4",
        "corruption": {
            "source_stem": "basic-h264-aac",
            "source_ext": "mp4",
            "type": "wrong_extension",
        },
    }

    result = generator.materialize_corrupt_fixture(tmp_path, spec, random.Random(7))

    final_path = tmp_path / "corrupt-wrong-extension.mkv"
    assert result["original_filename"] == "corrupt-wrong-extension.mp4"
    assert result["filename"] == "corrupt-wrong-extension.mkv"
    assert result["size"] == final_path.stat().st_size
    assert final_path.exists()
    assert not (tmp_path / result["original_filename"]).exists()


def test_ensure_ffmpeg_available_exits_when_binary_missing(
    generator, monkeypatch, capsys
):
    monkeypatch.setattr(generator.shutil, "which", lambda name: None)

    with pytest.raises(SystemExit) as exc_info:
        generator.ensure_ffmpeg_available()

    assert exc_info.value.code == 1
    assert capsys.readouterr().err == "Error: ffmpeg not found in PATH\n"


def test_ensure_ffmpeg_available_returns_when_binary_exists(generator, monkeypatch):
    monkeypatch.setattr(generator.shutil, "which", lambda name: "/usr/bin/ffmpeg")

    generator.ensure_ffmpeg_available()


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


def test_build_video_input_uses_mandelbrot_source_and_black_bars(generator):
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


def test_build_video_input_keeps_testsrc_for_smoke_fixture(generator):
    video = {"source": "testsrc2", "size": "1280x720", "fps": 24}

    source = generator.build_video_input(video, duration=2, specials=set())

    assert source == "testsrc2=duration=2:size=1280x720:rate=24"


def test_build_video_input_dark_edges_adds_vignette_without_black_bars(generator):
    video = {
        "source": "mandelbrot_zoom",
        "size": "1920x1080",
        "fps": 24,
    }

    source = generator.build_video_input(video, duration=2, specials={"dark_edges"})

    assert "vignette=PI/5" in source
    assert "pad=1920:1080:(ow-iw)/2:(oh-ih)/2:black" not in source


def test_build_audio_input_dynamic_bursts_uses_aevalsrc_expression(generator):
    source = generator.build_audio_input(
        {"source": "dynamic_bursts"},
        index=0,
        duration=2,
    )

    assert source.startswith("aevalsrc=")
    assert "between(mod(t,1),0,0.15)" in source
    assert "0.9*sin(2*PI*880*t)" in source
    assert "0.04*sin(2*PI*220*t)" in source


def test_build_ffmpeg_cmd_adds_loudnorm_filter_for_target_fixture(generator, tmp_path):
    spec = next(
        spec
        for spec in generator.build_manifest()
        if spec["stem"] == "loudness-normalized-target"
    )

    cmd, _ = generator.build_ffmpeg_cmd(
        spec,
        tmp_path,
        duration=2,
        rng=random.Random(1),
        tmpdir=tmp_path,
    )

    assert "-af:a:0" in cmd
    assert cmd[cmd.index("-af:a:0") + 1] == "loudnorm=I=-23:TP=-2:LRA=11"


def test_add_audio_codec_options_sets_explicit_stereo_for_two_channels(generator):
    cmd = []

    generator.add_audio_codec_options(
        cmd,
        [{"codec": "aac", "channels": 2}],
    )

    assert "-ac:a:0" in cmd
    assert cmd[cmd.index("-ac:a:0") + 1] == "2"
    assert "-channel_layout:a:0" in cmd
    assert cmd[cmd.index("-channel_layout:a:0") + 1] == "stereo"
