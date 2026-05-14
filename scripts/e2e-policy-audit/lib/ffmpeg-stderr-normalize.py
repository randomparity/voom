#!/usr/bin/env python3
"""Normalize ffmpeg stderr tails in exported plan result TSVs."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


FFMPEG_PROGRESS_RE = re.compile(r"^\s*frame=\s*\d+\s+fps=")


def normalized_stderr_tail(stderr_tail: str) -> str:
    lines = re.split(r"\r\n|\r|\n", stderr_tail)
    kept: list[str] = []
    removed_progress = False
    for line in lines:
        if FFMPEG_PROGRESS_RE.match(line):
            removed_progress = True
            continue
        kept.append(line)

    if not removed_progress:
        return stderr_tail

    while kept and kept[0] == "":
        kept.pop(0)
    while kept and kept[-1] == "":
        kept.pop()

    return "\n".join(kept)


def normalized_result(result: str) -> str:
    try:
        value = json.loads(result)
    except json.JSONDecodeError:
        return result

    if not isinstance(value, dict):
        return result

    detail = value.get("detail")
    if not isinstance(detail, dict):
        return result

    stderr_tail = detail.get("stderr_tail")
    if not isinstance(stderr_tail, str):
        return result

    normalized = normalized_stderr_tail(stderr_tail)
    if normalized == stderr_tail:
        return result

    detail["stderr_tail"] = normalized
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def split_line_ending(line: str) -> tuple[str, str]:
    if line.endswith("\r\n"):
        return line[:-2], "\r\n"
    if line.endswith("\n"):
        return line[:-1], "\n"
    return line, ""


def normalize_plans(input_path: Path, output_path: Path) -> None:
    with input_path.open(newline="") as src, output_path.open("w", newline="") as dst:
        header = src.readline()
        if not header:
            return

        dst.write(header)
        header_body, _ = split_line_ending(header)
        columns = header_body.split("\t")
        try:
            result_index = columns.index("result")
        except ValueError:
            for line in src:
                dst.write(line)
            return

        for line in src:
            body, ending = split_line_ending(line)
            fields = body.split("\t")
            if len(fields) <= result_index:
                dst.write(line)
                continue

            normalized = normalized_result(fields[result_index])
            if normalized == fields[result_index]:
                dst.write(line)
                continue

            fields[result_index] = normalized
            dst.write("\t".join(fields) + ending)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Strip ffmpeg carriage-return progress from plan stderr tails."
    )
    parser.add_argument("input_tsv")
    parser.add_argument("output_tsv")
    args = parser.parse_args()

    normalize_plans(Path(args.input_tsv), Path(args.output_tsv))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
