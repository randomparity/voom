#!/usr/bin/env python3
"""Render a video-codec × container pivot table for pre vs post NDJSON."""
from __future__ import annotations

import argparse
import json
import sys


def load_pivot(path: str) -> dict[tuple[str, str], int]:
    counts: dict[tuple[str, str], int] = {}
    with open(path) as f:
        for raw in f:
            raw = raw.strip()
            if not raw:
                continue
            obj = json.loads(raw)
            container = obj.get("container", "?")
            video = obj.get("video") or []
            codec = video[0]["codec"] if video else "(none)"
            counts[(codec, container)] = counts.get((codec, container), 0) + 1
    return counts


def render(pre: dict, post: dict) -> str:
    codecs = sorted({c for c, _ in pre} | {c for c, _ in post})
    containers = sorted({k for _, k in pre} | {k for _, k in post})
    lines = ["# Codec Pivot (video codec × container)", ""]
    for label, data in (("Pre", pre), ("Post", post)):
        lines.append(f"## {label}")
        lines.append("")
        header = "| codec | " + " | ".join(containers) + " | total |"
        sep = "|" + "|".join(["---"] * (len(containers) + 2)) + "|"
        lines.extend([header, sep])
        for codec in codecs:
            row = [codec]
            total = 0
            for c in containers:
                v = data.get((codec, c), 0)
                row.append(str(v))
                total += v
            row.append(str(total))
            lines.append("| " + " | ".join(row) + " |")
        lines.append("")
    return "\n".join(lines)


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("pre", help="pre-snapshot NDJSON")
    p.add_argument("post", help="post-snapshot NDJSON")
    p.add_argument("out", help="output markdown path")
    args = p.parse_args()

    pre = load_pivot(args.pre)
    post = load_pivot(args.post)
    with open(args.out, "w") as f:
        f.write(render(pre, post))
    return 0


if __name__ == "__main__":
    sys.exit(main())
