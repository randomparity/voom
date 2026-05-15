#!/usr/bin/env python3
"""Compare plan-only preview JSON with executed plans.tsv."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
from typing import Any


def load_json(path: Path) -> list[dict[str, Any]]:
    text = path.read_text().strip()
    if not text:
        return []
    data = json.loads(text)
    if not isinstance(data, list):
        raise SystemExit(f"{path}: expected a JSON array")
    return data


def file_paths(files_tsv: Path) -> dict[str, str]:
    with files_tsv.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        return {
            row["id"]: row["path"]
            for row in reader
            if row.get("id") and row.get("path")
        }


def stable_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def normalize_action(action: dict[str, Any]) -> str:
    fields = {key: value for key, value in action.items() if value is not None}
    parts: list[str] = []
    for key in sorted(fields):
        value = fields[key]
        if isinstance(value, (dict, list)):
            parts.append(f"{key}={stable_json(value)}")
        else:
            parts.append(f"{key}={value}")
    return ";".join(parts)


def action_signature(actions: Any) -> str:
    if isinstance(actions, str):
        actions = json.loads(actions or "[]")
    if not actions:
        return ""
    return " | ".join(normalize_action(action) for action in actions)


def preview_rows(preview_json: Path) -> dict[tuple[str, str], dict[str, str]]:
    rows: dict[tuple[str, str], dict[str, str]] = {}
    for item in load_json(preview_json):
        path = str((item.get("file") or {}).get("path") or item.get("path") or "")
        phase = str(item.get("phase_name") or item.get("phase") or "")
        if not path or not phase:
            continue
        rows[(path, phase)] = {
            "status": "skipped" if item.get("skip_reason") else "planned",
            "actions": action_signature(item.get("actions") or []),
            "skip_reason": str(item.get("skip_reason") or ""),
        }
    return rows


def executed_rows(plans_tsv: Path, files_tsv: Path) -> dict[tuple[str, str], dict[str, str]]:
    paths = file_paths(files_tsv)
    rows: dict[tuple[str, str], dict[str, str]] = {}
    with plans_tsv.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        for row in reader:
            path = paths.get(row.get("file_id", ""), "")
            phase = row.get("phase_name", "")
            if not path or not phase:
                continue
            rows[(path, phase)] = {
                "status": row.get("status", ""),
                "actions": action_signature(row.get("actions", "[]")),
                "skip_reason": row.get("skip_reason", ""),
            }
    return rows


def diff_rows(
    preview: dict[tuple[str, str], dict[str, str]],
    executed: dict[tuple[str, str], dict[str, str]],
) -> list[dict[str, str]]:
    out: list[dict[str, str]] = []
    for key in sorted(set(preview) | set(executed)):
        p = preview.get(key)
        e = executed.get(key)
        path, phase = key
        if p is None:
            out.append(
                {
                    "path": path,
                    "phase": phase,
                    "diff_class": "executed-only",
                    "preview": "",
                    "executed": e["status"],
                }
            )
            continue
        if e is None:
            out.append(
                {
                    "path": path,
                    "phase": phase,
                    "diff_class": "preview-only",
                    "preview": p["status"],
                    "executed": "",
                }
            )
            continue
        if p["skip_reason"] != e["skip_reason"]:
            out.append(
                {
                    "path": path,
                    "phase": phase,
                    "diff_class": "skip-reason",
                    "preview": p["skip_reason"],
                    "executed": e["skip_reason"],
                }
            )
        if p["actions"] != e["actions"]:
            out.append(
                {
                    "path": path,
                    "phase": phase,
                    "diff_class": "action-params",
                    "preview": p["actions"],
                    "executed": e["actions"],
                }
            )
    return out


def write_tsv(path: Path, rows: list[dict[str, str]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = ["path", "phase", "diff_class", "preview", "executed"]
    with path.open("w") as f:
        f.write("\t".join(fields) + "\n")
        for row in rows:
            f.write("\t".join(row[field].replace("\t", " ") for field in fields) + "\n")


def md_escape(value: str) -> str:
    return value.replace("|", "\\|")


def write_md(path: Path, rows: list[dict[str, str]]) -> None:
    with path.open("w") as f:
        f.write("# Plan Preview vs Executed\n\n")
        f.write(f"Divergences: {len(rows)}\n\n")
        if not rows:
            f.write("(none)\n")
            return
        f.write("| Path | Phase | Diff | Preview | Executed |\n")
        f.write("|---|---|---|---|---|\n")
        for row in rows[:100]:
            f.write(
                f"| `{md_escape(row['path'])}` | {md_escape(row['phase'])} | "
                f"{md_escape(row['diff_class'])} | {md_escape(row['preview'])} | "
                f"{md_escape(row['executed'])} |\n"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("preview_json")
    parser.add_argument("plans_tsv")
    parser.add_argument("files_tsv")
    parser.add_argument("out_tsv")
    parser.add_argument("out_md")
    args = parser.parse_args()
    rows = diff_rows(
        preview_rows(Path(args.preview_json)),
        executed_rows(Path(args.plans_tsv), Path(args.files_tsv)),
    )
    write_tsv(Path(args.out_tsv), rows)
    write_md(Path(args.out_md), rows)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
