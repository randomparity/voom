#!/usr/bin/env python3
"""Collect CLI warning lines from e2e logs."""

from __future__ import annotations

import argparse
from pathlib import Path


def collect(logs_dir: Path) -> list[tuple[str, int, str]]:
    rows: list[tuple[str, int, str]] = []
    for log in sorted(logs_dir.glob("*.log")):
        with log.open(errors="replace") as f:
            for number, line in enumerate(f, start=1):
                text = line.rstrip("\n")
                if text.startswith("warning:"):
                    rows.append((log.name, number, text))
    return rows


def write_md(path: Path, rows: list[tuple[str, int, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        f.write("# Deprecation Warnings\n\n")
        f.write(f"Warnings: {len(rows)}\n\n")
        if not rows:
            f.write("(none)\n")
            return
        f.write("| Log | Line | Warning |\n")
        f.write("|---|---:|---|\n")
        for log_name, line_number, warning in rows:
            escaped = warning.replace("|", "\\|")
            f.write(f"| {log_name} | {line_number} | {escaped} |\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("logs_dir")
    parser.add_argument("out_md")
    args = parser.parse_args()
    write_md(Path(args.out_md), collect(Path(args.logs_dir)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
