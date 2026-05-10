#!/usr/bin/env python3
"""Summarize canonical diff TSV rows by stable change signatures."""

from __future__ import annotations

import argparse
import csv
import re
from collections import Counter, defaultdict
from pathlib import Path


def classify(change_class: str, details: str) -> str:
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
    if change_class in {"bitrate", "size", "container", "duration", "content_hash", "mtime"}:
        return change_class
    if change_class in {"video", "audio", "subtitle", "attachment"}:
        fields = sorted(set(re.findall(r"#\d+\.([A-Za-z0-9_]+):", details)))
        if fields:
            return f"{change_class}-field-change:" + ",".join(fields[:4])
    if change_class == "-":
        return "path-presence"
    return change_class or "unknown"


def summarize(diff_tsv: Path, out_tsv: Path, out_md: Path) -> None:
    counts: Counter[tuple[str, str]] = Counter()
    samples: dict[tuple[str, str], str] = {}
    paths_by_key: defaultdict[tuple[str, str], set[str]] = defaultdict(set)

    with diff_tsv.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        if reader.fieldnames is None or not {"path", "change-class", "details"}.issubset(
            reader.fieldnames
        ):
            raise SystemExit(f"{diff_tsv}: missing required diff columns")
        for row in reader:
            change_class = row["change-class"]
            signature = classify(change_class, row["details"])
            key = (change_class, signature)
            counts[key] += 1
            paths_by_key[key].add(row["path"])
            samples.setdefault(key, row["path"])

    out_tsv.parent.mkdir(parents=True, exist_ok=True)
    with out_tsv.open("w", newline="") as f:
        writer = csv.writer(f, delimiter="\t", lineterminator="\n")
        writer.writerow(["change-class", "signature", "rows", "files", "sample_path"])
        for (change_class, signature), rows in sorted(
            counts.items(), key=lambda item: (-item[1], item[0])
        ):
            writer.writerow(
                [
                    change_class,
                    signature,
                    rows,
                    len(paths_by_key[(change_class, signature)]),
                    samples[(change_class, signature)],
                ]
            )

    with out_md.open("w") as f:
        f.write(f"# Diff Class Summary: {diff_tsv.name}\n\n")
        if not counts:
            f.write("(none)\n")
            return
        f.write("| Rows | Files | Class | Signature | Sample |\n")
        f.write("|---:|---:|---|---|---|\n")
        for (change_class, signature), rows in sorted(
            counts.items(), key=lambda item: (-item[1], item[0])
        ):
            sample = samples[(change_class, signature)].replace("|", "\\|")
            f.write(
                f"| {rows} | {len(paths_by_key[(change_class, signature)])} | "
                f"{change_class} | {signature} | `{sample}` |\n"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("diff_tsv")
    parser.add_argument("out_tsv")
    parser.add_argument("out_md")
    args = parser.parse_args()
    summarize(Path(args.diff_tsv), Path(args.out_tsv), Path(args.out_md))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
