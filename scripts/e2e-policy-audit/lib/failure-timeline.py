#!/usr/bin/env python3
"""Bucket failed plans by execution hour and primary failure signature."""

from __future__ import annotations

import argparse
import csv
import json
import re
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path


SIGNATURES = ("cuda-no-device", "cuda-oom", "filter-format", "other")


def result_text(result: str) -> str:
    try:
        value = json.loads(result)
    except json.JSONDecodeError:
        return result

    if not isinstance(value, dict):
        return str(value)

    parts: list[str] = []
    error = value.get("error")
    if isinstance(error, str):
        parts.append(error)

    detail = value.get("detail")
    if isinstance(detail, dict):
        stderr_tail = detail.get("stderr_tail")
        if isinstance(stderr_tail, str):
            parts.append(stderr_tail)
        command = detail.get("command")
        if isinstance(command, str):
            parts.append(command)

    return "\n".join(parts)


def classify(text: str) -> str:
    if "No device available for decoder" in text or "CUDA_ERROR_NO_DEVICE" in text:
        return "cuda-no-device"
    if "CUDA_ERROR_OUT_OF_MEMORY" in text or re.search(
        r"out\s+of\s+memory", text, re.IGNORECASE
    ):
        return "cuda-oom"
    if "Impossible to convert between the formats" in text:
        return "filter-format"
    return "other"


def bucket_hour(timestamp: str) -> str:
    normalized = timestamp.strip()
    if normalized.endswith("Z"):
        normalized = normalized[:-1] + "+00:00"

    parsed = datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    utc_hour = parsed.astimezone(UTC).replace(minute=0, second=0, microsecond=0)
    return utc_hour.isoformat().replace("+00:00", "Z")


def require_columns(fieldnames: list[str] | None) -> None:
    if fieldnames is None:
        raise SystemExit("plans TSV is missing a header")

    missing = [
        name
        for name in ("status", "result", "executed_at")
        if name not in fieldnames
    ]
    if missing:
        raise SystemExit(f"plans TSV missing required column(s): {', '.join(missing)}")


def failure_counts(input_path: Path) -> dict[str, Counter]:
    counts: dict[str, Counter] = defaultdict(Counter)
    with input_path.open(newline="") as handle:
        reader = csv.DictReader(handle, delimiter="\t")
        require_columns(reader.fieldnames)

        for row in reader:
            if row["status"] != "failed":
                continue
            hour = bucket_hour(row["executed_at"])
            signature = classify(result_text(row["result"]))
            counts[hour][signature] += 1

    return counts


def render(counts: dict[str, Counter]) -> str:
    lines = [
        "# Failure Timeline",
        "",
        "| Hour | cuda-no-device | cuda-oom | filter-format | other | Total |",
        "|---|---:|---:|---:|---:|---:|",
    ]

    if not counts:
        lines.append("| (none) | 0 | 0 | 0 | 0 | 0 |")
        return "\n".join(lines) + "\n"

    for hour in sorted(counts):
        counter = counts[hour]
        values = [counter[signature] for signature in SIGNATURES]
        total = sum(values)
        lines.append(
            f"| {hour} | {values[0]} | {values[1]} | {values[2]} | {values[3]} | {total} |"
        )

    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Bucket failed plans by executed_at hour and failure signature."
    )
    parser.add_argument("plans_tsv")
    parser.add_argument("output")
    args = parser.parse_args()

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(render(failure_counts(Path(args.plans_tsv))))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
