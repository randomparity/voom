#!/usr/bin/env python3
"""Build small file lists for reproducing e2e failures."""

from __future__ import annotations

import argparse
import csv
import json
import os
import re
from collections import defaultdict
from pathlib import Path


def manifest_library(run_dir: Path) -> str:
    manifest = run_dir / "manifest.json"
    if not manifest.exists():
        return ""
    with manifest.open() as f:
        return str(json.load(f).get("library") or "")


def diff_signature(change_class: str, details: str) -> str:
    if change_class == "video" and re.search(r"\+#\d+\(png\)", details):
        return "attachment-promoted-to-png-video"
    if change_class == "attachment" and re.search(r"-#\d+\(png\)", details):
        return "png-attachment-removed"
    if change_class == "subtitle" and ".is_default:False→True" in details:
        return "subtitle-default-enabled"
    if change_class == "subtitle" and ".is_default:True→False" in details:
        return "subtitle-default-disabled"
    if ".language:und→" in details:
        return "language-detected-from-und"
    if re.search(r"\.language:[^→]+→und", details):
        return "language-lost-to-und"
    if re.search(r"\+#\d+", details):
        return f"{change_class}-stream-added"
    if re.search(r"-#\d+", details):
        return f"{change_class}-stream-removed"
    if change_class in {"video", "audio", "subtitle", "attachment"}:
        fields = sorted(set(re.findall(r"#\d+\.([A-Za-z0-9_]+):", details)))
        if fields:
            return f"{change_class}-field-change:" + ",".join(fields[:4])
    return change_class or "unknown"


def add_failed_plan_rows(run_dir: Path, rows: list[dict[str, str]]) -> None:
    path = run_dir / "repro" / "failed-plan-files.tsv"
    if not path.exists():
        return
    with path.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for row in reader:
            if not row.get("path"):
                continue
            rows.append(
                {
                    "path": row["path"],
                    "source": "failed-plan",
                    "signature": row.get("signature", "unknown"),
                    "phase": row.get("phase", ""),
                    "detail": row.get("plan_id", ""),
                }
            )


def add_diff_rows(run_dir: Path, diff_name: str, rows: list[dict[str, str]]) -> None:
    path = run_dir / "diffs" / f"{diff_name}.tsv"
    if not path.exists():
        return
    with path.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for row in reader:
            if row.get("side") != "both":
                continue
            change_class = row.get("change-class", "")
            details = row.get("details", "")
            signature = diff_signature(change_class, details)
            if signature in {"bitrate", "size", "content_hash"}:
                continue
            rows.append(
                {
                    "path": row["path"],
                    "source": diff_name,
                    "signature": signature,
                    "phase": "",
                    "detail": details[:500],
                }
            )


def unique_rows(rows: list[dict[str, str]]) -> list[dict[str, str]]:
    seen: set[tuple[str, str, str]] = set()
    out: list[dict[str, str]] = []
    for row in rows:
        key = (row["path"], row["source"], row["signature"])
        if key in seen:
            continue
        seen.add(key)
        out.append(row)
    return out


def relative_to_library(path: str, library: str) -> str:
    if not library:
        return ""
    try:
        rel = os.path.relpath(path, library)
    except ValueError:
        return ""
    if rel == "." or rel.startswith(".."):
        return ""
    return rel


def write_tsv(path: Path, rows: list[dict[str, str]], *, library: str) -> None:
    with path.open("w", newline="") as f:
        fieldnames = ["path", "relative_path", "source", "signature", "phase", "detail"]
        writer = csv.DictWriter(f, fieldnames=fieldnames, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        for row in rows:
            out = dict(row)
            out["relative_path"] = relative_to_library(row["path"], library)
            writer.writerow(out)


def write_copy_script(run_dir: Path, library: str) -> None:
    script = run_dir / "repro" / "copy-repro-set.sh"
    script.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail

dest="${{1:?usage: $0 <dest-dir>}}"
library="${{VOOM_REPRO_LIBRARY:-{library}}}"
list_dir="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" && pwd)"
paths="${{list_dir}}/minimal-covering-set.relative-paths"

if [[ ! -d "${{library}}" ]]; then
  echo "library root not found: ${{library}}" >&2
  exit 1
fi

mkdir -p "${{dest}}"
rsync -a --files-from="${{paths}}" "${{library%/}}/" "${{dest%/}}/"
echo "Copied repro set to ${{dest}}"
"""
    )
    script.chmod(0o755)


def build(run_dir: Path, cap: int) -> None:
    repro_dir = run_dir / "repro"
    repro_dir.mkdir(parents=True, exist_ok=True)
    library = manifest_library(run_dir)

    rows: list[dict[str, str]] = []
    add_failed_plan_rows(run_dir, rows)
    add_diff_rows(run_dir, "db-vs-ffprobe-post", rows)
    add_diff_rows(run_dir, "ffprobe-pre-vs-post", rows)
    rows = unique_rows(rows)

    write_tsv(repro_dir / "all-problem-files.tsv", rows, library=library)

    selected: list[dict[str, str]] = []
    counts: defaultdict[tuple[str, str], int] = defaultdict(int)
    for row in rows:
        key = (row["source"], row["signature"])
        if counts[key] >= cap:
            continue
        selected.append(row)
        counts[key] += 1

    write_tsv(repro_dir / "minimal-covering-set.tsv", selected, library=library)

    absolute_paths = sorted({row["path"] for row in selected if row["path"]})
    (repro_dir / "minimal-covering-set.paths").write_text(
        "".join(f"{path}\n" for path in absolute_paths)
    )

    relative_paths = sorted(
        {
            relative_to_library(row["path"], library)
            for row in selected
            if relative_to_library(row["path"], library)
        }
    )
    (repro_dir / "minimal-covering-set.relative-paths").write_text(
        "".join(f"{path}\n" for path in relative_paths)
    )

    attachment_rows = [
        row
        for row in rows
        if "attachment" in row["signature"] or "png-video" in row["signature"]
    ]
    write_tsv(repro_dir / "attachment-regression-files.tsv", attachment_rows, library=library)

    metadata_rows = [
        row
        for row in rows
        if row["source"] != "failed-plan"
        and (
            "language" in row["signature"]
            or "default" in row["signature"]
            or "field-change" in row["signature"]
            or "stream-" in row["signature"]
        )
    ]
    write_tsv(repro_dir / "metadata-drift-files.tsv", metadata_rows, library=library)
    write_copy_script(run_dir, library)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("run_dir")
    parser.add_argument("--cap-per-signature", type=int, default=3)
    args = parser.parse_args()
    build(Path(args.run_dir), args.cap_per_signature)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
