"""Shared utilities for e2e-policy-audit Python scripts.

Underscore prefix marks this as harness-internal (not invoked directly).
"""
from __future__ import annotations

import json
import sys
from typing import Iterator


def stream_records(path: str) -> Iterator[dict]:
    """Yield each JSON record from an NDJSON file. Logs parse errors to stderr
    and continues."""
    with open(path) as f:
        for ln, raw in enumerate(f, 1):
            raw = raw.strip()
            if not raw:
                continue
            try:
                yield json.loads(raw)
            except json.JSONDecodeError as e:
                print(f"WARN: {path}:{ln} parse error: {e}", file=sys.stderr)


def load_keyed(path: str, key: str = "path") -> dict[str, dict]:
    """Read NDJSON; return dict keyed by the given top-level field."""
    return {r[key]: r for r in stream_records(path)}


def render_markdown_table(headers: list[str], rows: list[list[str]]) -> list[str]:
    """Render a markdown table as a list of lines (no trailing newline).

    `headers` is the column labels. `rows` is a list of row data; each row
    must have the same length as `headers`.
    """
    out = ["| " + " | ".join(headers) + " |",
           "|" + "|".join(["---"] * len(headers)) + "|"]
    for row in rows:
        out.append("| " + " | ".join(row) + " |")
    return out
