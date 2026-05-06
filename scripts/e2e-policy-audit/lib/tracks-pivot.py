#!/usr/bin/env python3
"""Render audio-codec × language and subtitle-codec × language pivots."""
from __future__ import annotations

import argparse
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _pyutil import render_markdown_table, stream_records  # noqa: E402


def load(path: str, bucket: str) -> dict[tuple[str, str], int]:
    counts: dict[tuple[str, str], int] = {}
    for obj in stream_records(path):
        for t in obj.get(bucket) or []:
            key = (t.get("codec", "?"), t.get("language", "und"))
            counts[key] = counts.get(key, 0) + 1
    return counts


def render_pivot(title: str, pre: dict, post: dict) -> list[str]:
    codecs = sorted({c for c, _ in pre} | {c for c, _ in post})
    langs = sorted({l for _, l in pre} | {l for _, l in post})
    out = [f"## {title}", ""]
    for label, data in (("Pre", pre), ("Post", post)):
        out.append(f"### {label}")
        out.append("")
        headers = ["codec"] + langs + ["total"]
        rows = []
        for codec in codecs:
            row = [codec]
            total = 0
            for l in langs:
                v = data.get((codec, l), 0)
                row.append(str(v))
                total += v
            row.append(str(total))
            rows.append(row)
        out.extend(render_markdown_table(headers, rows))
        out.append("")
    return out


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("pre")
    p.add_argument("post")
    p.add_argument("out")
    args = p.parse_args()

    lines = ["# Tracks Pivot", ""]
    lines.extend(render_pivot("Audio (codec × language)",
                              load(args.pre, "audio"),
                              load(args.post, "audio")))
    lines.extend(render_pivot("Subtitle (codec × language)",
                              load(args.pre, "subtitle"),
                              load(args.post, "subtitle")))
    with open(args.out, "w") as f:
        f.write("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
