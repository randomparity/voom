#!/usr/bin/env python3
"""Dedupe repeated plugin.error events while preserving first full payloads."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


UUID_RE = re.compile(
    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"
)
PATH_RE = re.compile(r"(?<!\S)/(?:[^\s:]+/?)+")
NUMBER_RE = re.compile(r"\b\d+(?:\.\d+)?\b")


def load_events(path: Path) -> list[dict[str, Any]]:
    with path.open() as f:
        events = json.load(f)
    if not isinstance(events, list):
        raise SystemExit(f"{path}: expected a JSON array")
    if not all(isinstance(event, dict) for event in events):
        raise SystemExit(f"{path}: expected every event to be an object")
    return events


def event_payload(event: dict[str, Any]) -> dict[str, Any]:
    payload = event.get("payload")
    return payload if isinstance(payload, dict) else {}


def plugin_name(event: dict[str, Any]) -> str:
    payload = event_payload(event)
    for key in ("plugin_name", "plugin", "name"):
        value = payload.get(key)
        if value:
            return str(value)
    summary = str(event.get("summary") or "")
    if ":" in summary:
        prefix = summary.split(":", 1)[0].strip()
        if prefix:
            return prefix
    return "unknown"


def first_payload_line(payload: dict[str, Any]) -> str:
    for key in ("error", "message", "detail", "stderr_tail"):
        value = payload.get(key)
        if value is None:
            continue
        if isinstance(value, (dict, list)):
            text = json.dumps(value, sort_keys=True)
        else:
            text = str(value)
        for line in text.splitlines():
            stripped = line.strip()
            if stripped:
                return stripped
    return "unknown"


def normalize_signature(line: str) -> str:
    signature = UUID_RE.sub("<uuid>", line)
    signature = PATH_RE.sub("<path>", signature)
    signature = NUMBER_RE.sub("<n>", signature)
    signature = re.sub(r"\s+", " ", signature).strip()
    return signature or "unknown"


def safe_log_name(plugin: str) -> str:
    name = re.sub(r"[^A-Za-z0-9._-]+", "-", plugin.strip()).strip("-")
    return name or "unknown"


def markdown_cell(value: Any) -> str:
    return str(value).replace("|", "\\|").replace("\n", " ")


def dedupe_events(
    events: list[dict[str, Any]],
    log_dir: Path,
) -> tuple[list[dict[str, Any]], dict[tuple[str, str], dict[str, Any]]]:
    groups: dict[tuple[str, str], dict[str, Any]] = {}
    output: list[dict[str, Any]] = []
    log_lines: dict[str, list[str]] = {}

    for event in events:
        if event.get("event_type") != "plugin.error":
            output.append(event)
            continue

        payload = event_payload(event)
        plugin = plugin_name(event)
        signature = normalize_signature(first_payload_line(payload))
        key = (plugin, signature)
        group = groups.setdefault(
            key,
            {
                "count": 0,
                "first_rowid": event.get("rowid"),
                "first_seen": event.get("created_at", ""),
                "last_seen": event.get("created_at", ""),
                "log": f"logs/plugin-errors/{safe_log_name(plugin)}.log",
            },
        )
        group["count"] += 1
        group["last_seen"] = event.get("created_at", "")

        log_lines.setdefault(plugin, []).append(
            f"{event.get('created_at', '')}\tcount={group['count']}\t"
            f"signature={signature}\trowid={event.get('rowid', '')}"
        )

        if group["count"] == 1:
            output.append(event)
            continue

        compact = dict(event)
        compact["event_type"] = "plugin.error.deduped"
        compact["summary"] = (
            f"{plugin} duplicate plugin.error signature seen {group['count']} times"
        )
        compact["payload"] = {
            "plugin_name": plugin,
            "signature": signature,
            "duplicate_count": group["count"],
            "first_rowid": group["first_rowid"],
            "log": group["log"],
        }
        output.append(compact)

    log_dir.mkdir(parents=True, exist_ok=True)
    for plugin, lines in sorted(log_lines.items()):
        (log_dir / f"{safe_log_name(plugin)}.log").write_text("\n".join(lines) + "\n")

    return output, groups


def render_summary(groups: dict[tuple[str, str], dict[str, Any]]) -> str:
    lines = [
        "# Plugin Error Summary",
        "",
        "| Plugin | Signature | Count | First Row | First Seen | Last Seen |",
        "|---|---|---:|---:|---|---|",
    ]
    for (plugin, signature), group in sorted(
        groups.items(), key=lambda item: (-item[1]["count"], item[0][0], item[0][1])
    ):
        lines.append(
            "| "
            + " | ".join(
                [
                    markdown_cell(plugin),
                    markdown_cell(signature),
                    str(group["count"]),
                    markdown_cell(group["first_rowid"]),
                    markdown_cell(group["first_seen"]),
                    markdown_cell(group["last_seen"]),
                ]
            )
            + " |"
        )
    lines.extend(
        [
            "",
            "Repeated payload details were written to `logs/plugin-errors/`.",
        ]
    )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Dedupe plugin.error events.")
    parser.add_argument("events_json")
    parser.add_argument("deduped_json")
    parser.add_argument("plugin_error_log_dir")
    parser.add_argument("summary_md")
    args = parser.parse_args()

    events = load_events(Path(args.events_json))
    deduped, groups = dedupe_events(events, Path(args.plugin_error_log_dir))

    deduped_path = Path(args.deduped_json)
    deduped_path.parent.mkdir(parents=True, exist_ok=True)
    deduped_path.write_text(json.dumps(deduped, indent=2) + "\n")

    summary_path = Path(args.summary_md)
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    summary_path.write_text(render_summary(groups))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
