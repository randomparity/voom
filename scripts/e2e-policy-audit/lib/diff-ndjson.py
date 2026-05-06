#!/usr/bin/env python3
"""Generic NDJSON differ for the policy-audit harness.

Joins two canonical-NDJSON files on `path`, emits one TSV row per file that
differs, with a human-readable change description per affected field.

Output TSV columns:
    path  side  change-class  details

`side` is one of: left-only | right-only | both.
For `both`, one row is emitted per *changed top-level field* (container,
duration, bitrate, video, audio, subtitle, attachment, content_hash, mtime).
Fields where either side is null/missing are skipped (not a meaningful diff).
Fields listed in `--ignore-file` are skipped wholesale.
"""
from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from _pyutil import load_keyed  # noqa: E402


@dataclass
class IgnoreSpec:
    fields: set[str]            # e.g. {"mtime"} — top-level scalars
    track_fields: dict[str, set[str]]  # e.g. {"audio": {"track_type"}}

    @classmethod
    def parse(cls, text: str) -> "IgnoreSpec":
        fields: set[str] = set()
        track_fields: dict[str, set[str]] = {}
        track_re = re.compile(r"^(video|audio|subtitle|attachment)\[\]\.(\w+)$")
        for raw in text.splitlines():
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            m = track_re.match(line)
            if m:
                bucket, field = m.group(1), m.group(2)
                track_fields.setdefault(bucket, set()).add(field)
            else:
                fields.add(line)
        return cls(fields=fields, track_fields=track_fields)


def load_ndjson(path: str) -> dict[str, dict]:
    """Read NDJSON; return dict keyed by record['path']."""
    return load_keyed(path)


def both_present(a, b) -> bool:
    return a is not None and b is not None


def duration_close(a: float, b: float) -> bool:
    if a == 0 and b == 0:
        return True
    bigger = max(abs(a), abs(b))
    return abs(a - b) / bigger <= 0.005  # within 0.5%


def diff_tracks(left: list[dict], right: list[dict], ignore: set[str]) -> str | None:
    """Return human-readable diff string or None if identical."""
    by_idx_l = {t["index"]: t for t in left}
    by_idx_r = {t["index"]: t for t in right}
    all_idx = sorted(set(by_idx_l) | set(by_idx_r))
    parts: list[str] = []
    for idx in all_idx:
        l = by_idx_l.get(idx)
        r = by_idx_r.get(idx)
        if l is None:
            parts.append(f"+#{idx}({r.get('codec','?')})")
            continue
        if r is None:
            parts.append(f"-#{idx}({l.get('codec','?')})")
            continue
        for k in sorted(set(l) | set(r)):
            if k == "index" or k in ignore:
                continue
            lv = l.get(k); rv = r.get(k)
            if not both_present(lv, rv):
                continue
            if lv != rv:
                parts.append(f"#{idx}.{k}:{lv}→{rv}")
    return "; ".join(parts) if parts else None


def diff_record(l: dict, r: dict, ignore: IgnoreSpec) -> list[tuple[str, str]]:
    """Return list of (change-class, details) for a left/right record pair."""
    rows: list[tuple[str, str]] = []
    scalar_keys = ["container", "duration", "bitrate", "content_hash", "size", "mtime"]
    for k in scalar_keys:
        if k in ignore.fields:
            continue
        lv = l.get(k); rv = r.get(k)
        if not both_present(lv, rv):
            continue
        if k == "duration" and isinstance(lv, (int, float)) and isinstance(rv, (int, float)):
            if duration_close(float(lv), float(rv)):
                continue
            rows.append((k, f"{lv} → {rv} ({(rv-lv)/max(lv,1e-9)*100:+.2f}%)"))
        elif lv != rv:
            rows.append((k, f"{lv} → {rv}"))

    for bucket in ("video", "audio", "subtitle", "attachment"):
        if bucket in ignore.fields:
            continue
        ignored_track_fields = ignore.track_fields.get(bucket, set())
        d = diff_tracks(l.get(bucket, []) or [], r.get(bucket, []) or [], ignored_track_fields)
        if d is not None:
            rows.append((bucket, d))
    return rows


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("left", help="left-side NDJSON")
    p.add_argument("right", help="right-side NDJSON")
    p.add_argument("out", help="output TSV path")
    p.add_argument("--ignore-file", required=True, help="path to ndjson-ignore.txt")
    args = p.parse_args()

    with open(args.ignore_file) as f:
        ignore = IgnoreSpec.parse(f.read())

    left = load_ndjson(args.left)
    right = load_ndjson(args.right)
    paths = sorted(set(left) | set(right))

    with open(args.out, "w") as f:
        f.write("path\tside\tchange-class\tdetails\n")
        for path in paths:
            l = left.get(path); r = right.get(path)
            if l is None:
                f.write(f"{path}\tright-only\t-\t-\n")
                continue
            if r is None:
                f.write(f"{path}\tleft-only\t-\t-\n")
                continue
            for change_class, details in diff_record(l, r, ignore):
                f.write(f"{path}\tboth\t{change_class}\t{details}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
