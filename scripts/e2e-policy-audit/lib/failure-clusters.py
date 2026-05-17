#!/usr/bin/env python3
"""Cluster failed plan rows into reviewable failure signatures."""

from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


def load_files(files_tsv: Path | None) -> dict[str, str]:
    if files_tsv is None or not files_tsv.exists():
        return {}
    with files_tsv.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        if reader.fieldnames is None or not {"id", "path"}.issubset(reader.fieldnames):
            return {}
        return {row["id"]: row["path"] for row in reader}


def load_ffprobe(path: Path | None) -> dict[str, dict[str, Any]]:
    if path is None or not path.exists():
        return {}
    records: dict[str, dict[str, Any]] = {}
    with path.open() as f:
        for line in f:
            if not line.strip():
                continue
            record = json.loads(line)
            records[record["path"]] = record
    return records


def text_for_result(raw: str) -> tuple[dict[str, Any], str]:
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {}, raw
    if not isinstance(parsed, dict):
        return {}, raw
    detail = parsed.get("detail") or {}
    parts = [
        str(parsed.get("error") or ""),
        str(detail.get("stderr_full") or detail.get("stderr_tail") or ""),
        str(detail.get("command") or ""),
    ]
    return parsed, "\n".join(parts)


def classify(text: str) -> str:
    checks = [
        ("storage-fk-transcode-outcome", "failed to insert transcode outcome: FOREIGN KEY"),
        ("cuda-context-oom", "CUDA_ERROR_OUT_OF_MEMORY"),
        ("cuda-context-unknown", "CUDA_ERROR_UNKNOWN"),
        ("nvenc-initialize-oom", "InitializeEncoder failed: out of memory"),
        ("cuda-decoder-packet-oom", "Error submitting packet to decoder: Cannot allocate memory"),
        ("filter-format-conversion", "Impossible to convert between the formats"),
        ("no-hw-decoder-device", "No device available for decoder"),
        ("no-output-written", "Nothing was written into output file"),
        ("ffmpeg-conversion-failed", "Conversion failed"),
    ]
    for signature, needle in checks:
        if needle in text:
            return signature
    if "ffmpeg exited with exit status" in text:
        return "ffmpeg-nonzero"
    return "unknown"


def exit_code(parsed: dict[str, Any]) -> str:
    detail = parsed.get("detail") or {}
    value = detail.get("exit_code")
    return "" if value is None else str(value)


def command(parsed: dict[str, Any]) -> str:
    detail = parsed.get("detail") or {}
    return str(detail.get("command") or "")


def video_shape(record: dict[str, Any] | None) -> tuple[str, str, str, str]:
    if not record:
        return "", "", "", ""
    videos = record.get("video") or []
    first = videos[0] if videos else {}
    codec = str(first.get("codec") or "")
    width = first.get("width")
    height = first.get("height")
    resolution = f"{width}x{height}" if width and height else ""
    container = str(record.get("container") or "")
    return container, codec, resolution, str(len(record.get("audio") or []))


def sample_error(text: str) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    interesting = [
        line
        for line in lines
        if any(
            needle in line
            for needle in [
                "CUDA_ERROR",
                "InitializeEncoder",
                "FOREIGN KEY",
                "No device available",
                "Impossible to convert",
                "Nothing was written",
                "Conversion failed",
                "ffmpeg exited",
            ]
        )
    ]
    chosen = interesting[:3] or lines[:3]
    return " | ".join(chosen)[:500]


def write_outputs(
    failed_plans: Path,
    files_tsv: Path | None,
    pre_ffprobe: Path | None,
    out_tsv: Path,
    out_md: Path,
) -> None:
    file_paths = load_files(files_tsv)
    ffprobe = load_ffprobe(pre_ffprobe)
    clusters: dict[tuple[str, str, str, str, str], dict[str, Any]] = {}
    per_file_rows: list[dict[str, str]] = []

    with failed_plans.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        if reader.fieldnames is None or not {"plan_id", "file_id", "phase", "result"}.issubset(
            reader.fieldnames
        ):
            raise SystemExit(f"{failed_plans}: missing required failed-plan columns")

        for row in reader:
            parsed, text = text_for_result(row["result"])
            path = file_paths.get(row["file_id"]) or extract_path(command(parsed)) or ""
            container, codec, resolution, audio_count = video_shape(ffprobe.get(path))
            signature = classify(text)
            code = exit_code(parsed)
            phase = row["phase"]
            key = (phase, signature, code, container, codec)
            cluster = clusters.setdefault(
                key,
                {
                    "count": 0,
                    "sample_path": path,
                    "sample_plan_id": row["plan_id"],
                    "sample_error": sample_error(text),
                    "resolutions": Counter(),
                    "audio_counts": Counter(),
                },
            )
            cluster["count"] += 1
            if resolution:
                cluster["resolutions"][resolution] += 1
            if audio_count:
                cluster["audio_counts"][audio_count] += 1
            per_file_rows.append(
                {
                    "path": path,
                    "phase": phase,
                    "signature": signature,
                    "exit_code": code,
                    "container": container,
                    "video_codec": codec,
                    "resolution": resolution,
                    "plan_id": row["plan_id"],
                    "file_id": row["file_id"],
                }
            )

    out_tsv.parent.mkdir(parents=True, exist_ok=True)
    with out_tsv.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t", lineterminator="\n")
        writer.writerow(
            [
                "phase",
                "signature",
                "exit_code",
                "container",
                "video_codec",
                "count",
                "top_resolution",
                "sample_path",
                "sample_plan_id",
                "sample_error",
            ]
        )
        for key, value in sorted(clusters.items(), key=lambda item: (-item[1]["count"], item[0])):
            phase, signature, code, container, codec = key
            top_resolution = ""
            if value["resolutions"]:
                top_resolution = value["resolutions"].most_common(1)[0][0]
            writer.writerow(
                [
                    phase,
                    signature,
                    code,
                    container,
                    codec,
                    value["count"],
                    top_resolution,
                    value["sample_path"],
                    value["sample_plan_id"],
                    value["sample_error"],
                ]
            )

    with out_md.open("w") as f:
        f.write("# Failure Clusters\n\n")
        if not clusters:
            f.write("(none)\n")
            return
        f.write("| Count | Phase | Signature | Exit | Source | Top Resolution | Sample |\n")
        f.write("|---:|---|---|---|---|---|---|\n")
        for key, value in sorted(clusters.items(), key=lambda item: (-item[1]["count"], item[0])):
            phase, signature, code, container, codec = key
            source = "/".join(part for part in [container, codec] if part)
            top_resolution = ""
            if value["resolutions"]:
                top_resolution = value["resolutions"].most_common(1)[0][0]
            sample = value["sample_path"].replace("|", "\\|")
            f.write(
                f"| {value['count']} | {phase} | {signature} | {code} | "
                f"{source} | {top_resolution} | `{sample}` |\n"
            )

    run_root = out_tsv.parent.parent if out_tsv.parent.name == "diffs" else out_tsv.parent
    repro_tsv = run_root / "repro" / "failed-plan-files.tsv"
    repro_tsv.parent.mkdir(parents=True, exist_ok=True)
    with repro_tsv.open("w", newline="") as f:
        writer = csv.DictWriter(
            f,
            fieldnames=[
                "path",
                "phase",
                "signature",
                "exit_code",
                "container",
                "video_codec",
                "resolution",
                "plan_id",
                "file_id",
            ],
            delimiter="\t",
            lineterminator="\n",
        )
        writer.writeheader()
        writer.writerows(per_file_rows)


def extract_path(command_text: str) -> str:
    match = re.search(r" -i '([^']+)' ", command_text)
    if match:
        return match.group(1)
    match = re.search(r' -i "([^"]+)" ', command_text)
    if match:
        return match.group(1)
    return ""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("failed_plans")
    parser.add_argument("out_tsv")
    parser.add_argument("out_md")
    parser.add_argument("--files-tsv")
    parser.add_argument("--pre-ffprobe")
    args = parser.parse_args()

    write_outputs(
        Path(args.failed_plans),
        Path(args.files_tsv) if args.files_tsv else None,
        Path(args.pre_ffprobe) if args.pre_ffprobe else None,
        Path(args.out_tsv),
        Path(args.out_md),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
