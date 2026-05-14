#!/usr/bin/env python3
"""Normalize ffmpeg stderr tails in exported plan result TSVs."""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path


FFMPEG_PROGRESS_RE = re.compile(r"^\s*frame=\s*\d+\s+fps=")


def normalized_stderr_tail(stderr_tail: str) -> str:
    lines = re.split(r"[\r\n]", stderr_tail)
    kept = [line for line in lines if not FFMPEG_PROGRESS_RE.match(line)]
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


def normalize_plans(input_path: Path, output_path: Path) -> None:
    with input_path.open(newline="") as src, output_path.open("w", newline="") as dst:
        reader = csv.DictReader(src, delimiter="\t")
        if reader.fieldnames is None:
            dst.write("")
            return

        writer = csv.DictWriter(
            dst,
            fieldnames=reader.fieldnames,
            delimiter="\t",
            lineterminator="\n",
        )
        writer.writeheader()

        for row in reader:
            if "result" in row and row["result"] is not None:
                row["result"] = normalized_result(row["result"])
            writer.writerow(row)


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
