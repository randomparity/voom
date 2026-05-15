# Issue 400 E2E Signal To Noise Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve `scripts/e2e-policy-audit` artifacts so repeated plugin failures, ffmpeg progress spam, time-localized failure bursts, and shallow web smoke checks are easy to interpret during pre-release verification.

**Architecture:** Keep the change script-only, as requested in the issue. Add small Python post-processors for event-log dedupe and failure timeline generation, wire them into the existing `run.sh`/`build-summary.sh` artifact flow, and harden `web-smoke.sh` with JSON/SSE/body assertions using `jq`, `sqlite3`, `curl`, and Bash.

**Tech Stack:** Bash with `set -euo pipefail` for orchestration and smoke checks. Python 3 stdlib for deterministic JSON/TSV processing. Existing `scripts/e2e-policy-audit/tests/test.sh` fixture style for tests.

**Issue:** https://github.com/randomparity/voom/issues/400

---

## File Map

- Create: `scripts/e2e-policy-audit/lib/plugin-error-dedupe.py` to parse `reports/events.json`, write `reports/events-deduped.json`, write repeated payload rollups under `logs/plugin-errors/`, and write `diffs/plugin-error-summary.md`.
- Create: `scripts/e2e-policy-audit/lib/ffmpeg-stderr-normalize.py` to strip carriage-return progress updates from `stderr_tail` fields in plan result JSON.
- Create: `scripts/e2e-policy-audit/lib/failure-timeline.py` to bucket failed plans by `executed_at` hour and primary signature.
- Modify: `scripts/e2e-policy-audit/run.sh` to create `logs/plugin-errors/`, run event dedupe after `voom events`, run stderr normalization before diffing plan exports, and generate `diffs/failure-timeline.md`.
- Modify: `scripts/e2e-policy-audit/lib/build-summary.sh` to link and preview `plugin-error-summary.md`, `failure-timeline.md`, and `events-deduped.json`.
- Modify: `scripts/e2e-policy-audit/lib/web-smoke.sh` to assert content shape for root, `/api/files?limit=1`, `/api/jobs?status=failed`, and `/events`.
- Modify: `scripts/e2e-policy-audit/README.md` to document the new artifacts and stronger web smoke assertions.
- Modify: `scripts/e2e-policy-audit/tests/test.sh` to add focused tests for the new Python scripts and web-smoke validation helpers.
- Create: `scripts/e2e-policy-audit/tests/expected/plugin-error-summary.md`.
- Create: `scripts/e2e-policy-audit/tests/expected/events-deduped.json`.
- Create: `scripts/e2e-policy-audit/tests/expected/plugin-errors/backup-manager.log`.
- Create: `scripts/e2e-policy-audit/tests/expected/failure-timeline.md`.

## Task 1: Plugin Error Dedupe

**Files:**
- Create: `scripts/e2e-policy-audit/lib/plugin-error-dedupe.py`
- Modify: `scripts/e2e-policy-audit/tests/test.sh`
- Create: `scripts/e2e-policy-audit/tests/expected/plugin-error-summary.md`
- Create: `scripts/e2e-policy-audit/tests/expected/events-deduped.json`
- Create: `scripts/e2e-policy-audit/tests/expected/plugin-errors/backup-manager.log`

- [ ] **Step 1: Add the failing test**

Add this function near the other focused tests in `scripts/e2e-policy-audit/tests/test.sh`, before the raw SQLite section:

```bash
run_plugin_error_dedupe_test() {
  local actual
  actual=$(mktemp -d)
  trap 'rm -R "${actual}"' EXIT
  mkdir -p "${actual}/reports" "${actual}/diffs" "${actual}/logs/plugin-errors"

  cat >"${actual}/reports/events.json" <<'EOF'
[
  {
    "rowid": 1,
    "event_type": "plugin.error",
    "created_at": "2026-05-14T10:00:01Z",
    "summary": "backup-manager: rclone failed",
    "payload": {
      "plugin_name": "backup-manager",
      "error": "rclone copy failed\nTransferred: 0 / 1, 0%\nERROR : /lib/a.mkv: permission denied"
    }
  },
  {
    "rowid": 2,
    "event_type": "plugin.error",
    "created_at": "2026-05-14T10:00:02Z",
    "summary": "backup-manager: rclone failed",
    "payload": {
      "plugin_name": "backup-manager",
      "error": "rclone copy failed\nTransferred: 0 / 1, 0%\nERROR : /lib/b.mkv: permission denied"
    }
  },
  {
    "rowid": 3,
    "event_type": "plan.completed",
    "created_at": "2026-05-14T10:00:03Z",
    "summary": "plan completed",
    "payload": {"plan_id": "plan-1"}
  },
  {
    "rowid": 4,
    "event_type": "plugin.error",
    "created_at": "2026-05-14T10:05:00Z",
    "summary": "backup-manager: config missing",
    "payload": {
      "plugin": "backup-manager",
      "message": "config missing\nrclone remote not configured"
    }
  }
]
EOF

  "lib/plugin-error-dedupe.py" \
    "${actual}/reports/events.json" \
    "${actual}/reports/events-deduped.json" \
    "${actual}/logs/plugin-errors" \
    "${actual}/diffs/plugin-error-summary.md"

  assert_match "${actual}/reports/events-deduped.json" "tests/expected/events-deduped.json"
  assert_match "${actual}/diffs/plugin-error-summary.md" "tests/expected/plugin-error-summary.md"
  assert_match "${actual}/logs/plugin-errors/backup-manager.log" \
    "tests/expected/plugin-errors/backup-manager.log"

  rm -R "${actual}"
  trap - EXIT
}

run_plugin_error_dedupe_test
```

Create `scripts/e2e-policy-audit/tests/expected/events-deduped.json`:

```json
[
  {
    "rowid": 1,
    "event_type": "plugin.error",
    "created_at": "2026-05-14T10:00:01Z",
    "summary": "backup-manager: rclone failed",
    "payload": {
      "plugin_name": "backup-manager",
      "error": "rclone copy failed\nTransferred: 0 / 1, 0%\nERROR : /lib/a.mkv: permission denied"
    }
  },
  {
    "rowid": 2,
    "event_type": "plugin.error.deduped",
    "created_at": "2026-05-14T10:00:02Z",
    "summary": "backup-manager duplicate plugin.error signature seen 2 times",
    "payload": {
      "plugin_name": "backup-manager",
      "signature": "rclone copy failed",
      "duplicate_count": 2,
      "first_rowid": 1,
      "log": "logs/plugin-errors/backup-manager.log"
    }
  },
  {
    "rowid": 3,
    "event_type": "plan.completed",
    "created_at": "2026-05-14T10:00:03Z",
    "summary": "plan completed",
    "payload": {
      "plan_id": "plan-1"
    }
  },
  {
    "rowid": 4,
    "event_type": "plugin.error",
    "created_at": "2026-05-14T10:05:00Z",
    "summary": "backup-manager: config missing",
    "payload": {
      "plugin": "backup-manager",
      "message": "config missing\nrclone remote not configured"
    }
  }
]
```

Create `scripts/e2e-policy-audit/tests/expected/plugin-error-summary.md`:

```markdown
# Plugin Error Summary

| Plugin | Signature | Count | First Row | First Seen | Last Seen |
|---|---|---:|---:|---|---|
| backup-manager | rclone copy failed | 2 | 1 | 2026-05-14T10:00:01Z | 2026-05-14T10:00:02Z |
| backup-manager | config missing | 1 | 4 | 2026-05-14T10:05:00Z | 2026-05-14T10:05:00Z |

Repeated payload details were written to `logs/plugin-errors/`.
```

Create `scripts/e2e-policy-audit/tests/expected/plugin-errors/backup-manager.log`:

```text
2026-05-14T10:00:01Z	count=1	signature=rclone copy failed	rowid=1
2026-05-14T10:00:02Z	count=2	signature=rclone copy failed	rowid=2
2026-05-14T10:05:00Z	count=1	signature=config missing	rowid=4
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: FAIL because `lib/plugin-error-dedupe.py` does not exist.

- [ ] **Step 3: Implement `plugin-error-dedupe.py`**

Create `scripts/e2e-policy-audit/lib/plugin-error-dedupe.py`:

```python
#!/usr/bin/env python3
"""Dedupe noisy plugin.error events into compact review artifacts."""

from __future__ import annotations

import argparse
import json
import re
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class SignatureState:
    plugin: str
    signature: str
    count: int
    first_rowid: int
    first_seen: str
    last_seen: str


def load_events(path: Path) -> list[dict[str, Any]]:
    with path.open() as f:
        data = json.load(f)
    if not isinstance(data, list):
        raise SystemExit(f"{path}: expected a JSON array")
    return [event for event in data if isinstance(event, dict)]


def payload_text(payload: Any) -> str:
    if isinstance(payload, str):
        return payload
    if not isinstance(payload, dict):
        return json.dumps(payload, sort_keys=True)
    for key in ("error", "message", "detail", "stderr_tail"):
        value = payload.get(key)
        if value:
            return str(value)
    return json.dumps(payload, sort_keys=True)


def plugin_name(event: dict[str, Any]) -> str:
    payload = event.get("payload")
    if isinstance(payload, dict):
        for key in ("plugin_name", "plugin", "name"):
            value = payload.get(key)
            if value:
                return safe_name(str(value))
    summary = str(event.get("summary") or "")
    if ":" in summary:
        return safe_name(summary.split(":", 1)[0].strip())
    return "unknown-plugin"


def safe_name(value: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9_.-]+", "-", value.strip())
    return cleaned.strip("-") or "unknown-plugin"


def primary_signature(text: str) -> str:
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        stripped = re.sub(r"/[^ \t:]+", "/<path>", stripped)
        stripped = re.sub(r"[0-9a-fA-F]{8}-[0-9a-fA-F-]{27,}", "<uuid>", stripped)
        stripped = re.sub(r"\b\d+(?:\.\d+)?\b", "<n>", stripped)
        return stripped[:160]
    return "(empty plugin error)"


def rel_log_path(plugin: str) -> str:
    return f"logs/plugin-errors/{plugin}.log"


def dedupe(
    events: list[dict[str, Any]],
    plugin_log_dir: Path,
) -> tuple[list[dict[str, Any]], OrderedDict[tuple[str, str], SignatureState]]:
    states: OrderedDict[tuple[str, str], SignatureState] = OrderedDict()
    deduped: list[dict[str, Any]] = []
    plugin_log_dir.mkdir(parents=True, exist_ok=True)
    log_handles: dict[str, Any] = {}

    try:
        for event in events:
            if event.get("event_type") != "plugin.error":
                deduped.append(event)
                continue

            plugin = plugin_name(event)
            text = payload_text(event.get("payload"))
            signature = primary_signature(text)
            rowid = int(event.get("rowid") or 0)
            created_at = str(event.get("created_at") or "")
            key = (plugin, signature)
            state = states.get(key)
            if state is None:
                state = SignatureState(
                    plugin=plugin,
                    signature=signature,
                    count=0,
                    first_rowid=rowid,
                    first_seen=created_at,
                    last_seen=created_at,
                )
                states[key] = state

            state.count += 1
            state.last_seen = created_at
            handle = log_handles.get(plugin)
            if handle is None:
                handle = (plugin_log_dir / f"{plugin}.log").open("w")
                log_handles[plugin] = handle
            handle.write(
                f"{created_at}\tcount={state.count}\tsignature={signature}\trowid={rowid}\n"
            )

            if state.count == 1:
                deduped.append(event)
            else:
                compact = dict(event)
                compact["event_type"] = "plugin.error.deduped"
                compact["summary"] = (
                    f"{plugin} duplicate plugin.error signature seen {state.count} times"
                )
                compact["payload"] = {
                    "plugin_name": plugin,
                    "signature": signature,
                    "duplicate_count": state.count,
                    "first_rowid": state.first_rowid,
                    "log": rel_log_path(plugin),
                }
                deduped.append(compact)
    finally:
        for handle in log_handles.values():
            handle.close()

    return deduped, states


def write_summary(path: Path, states: OrderedDict[tuple[str, str], SignatureState]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        f.write("# Plugin Error Summary\n\n")
        if not states:
            f.write("(none)\n")
            return
        f.write("| Plugin | Signature | Count | First Row | First Seen | Last Seen |\n")
        f.write("|---|---|---:|---:|---|---|\n")
        for state in sorted(states.values(), key=lambda s: (-s.count, s.plugin, s.signature)):
            signature = state.signature.replace("|", "\\|")
            f.write(
                f"| {state.plugin} | {signature} | {state.count} | {state.first_rowid} | "
                f"{state.first_seen} | {state.last_seen} |\n"
            )
        f.write("\nRepeated payload details were written to `logs/plugin-errors/`.\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("events_json")
    parser.add_argument("events_deduped_json")
    parser.add_argument("plugin_log_dir")
    parser.add_argument("summary_md")
    args = parser.parse_args()

    events = load_events(Path(args.events_json))
    deduped, states = dedupe(events, Path(args.plugin_log_dir))
    out = Path(args.events_deduped_json)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as f:
        json.dump(deduped, f, indent=2)
        f.write("\n")
    write_summary(Path(args.summary_md), states)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 4: Make it executable and rerun the focused test**

Run:

```bash
chmod +x scripts/e2e-policy-audit/lib/plugin-error-dedupe.py
scripts/e2e-policy-audit/tests/test.sh
```

Expected: PASS for `run_plugin_error_dedupe_test`; remaining pre-existing tests keep their previous result.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-policy-audit/lib/plugin-error-dedupe.py \
  scripts/e2e-policy-audit/tests/test.sh \
  scripts/e2e-policy-audit/tests/expected/events-deduped.json \
  scripts/e2e-policy-audit/tests/expected/plugin-error-summary.md \
  scripts/e2e-policy-audit/tests/expected/plugin-errors/backup-manager.log
git commit -m "scripts: dedupe e2e plugin error events"
```

## Task 2: FFmpeg `stderr_tail` Normalizer

**Files:**
- Create: `scripts/e2e-policy-audit/lib/ffmpeg-stderr-normalize.py`
- Modify: `scripts/e2e-policy-audit/tests/test.sh`

- [ ] **Step 1: Add the failing test**

Add this function to `scripts/e2e-policy-audit/tests/test.sh` after `run_plugin_error_dedupe_test`:

```bash
run_ffmpeg_stderr_normalize_test() {
  local actual
  actual=$(mktemp -d)
  trap 'rm -R "${actual}"' EXIT

  cat >"${actual}/plans.tsv" <<'EOF'
id	file_id	phase_name	status	result	executed_at
plan-1	file-1	transcode-video	failed	{"detail":{"stderr_tail":"frame= 1 fps=0.0 q=28.0 size=1k time=00:00:01 bitrate=8.0kbits/s speed=1x\rframe= 2 fps=1.0 q=28.0 size=2k time=00:00:02 bitrate=8.0kbits/s speed=1x\rCUDA_ERROR_OUT_OF_MEMORY\nConversion failed"},"error":"ffmpeg exited with exit status: 187"}	2026-05-14T10:00:00Z
plan-2	file-2	transcode-video	completed	{"ok":true}	2026-05-14T10:01:00Z
EOF

  "lib/ffmpeg-stderr-normalize.py" "${actual}/plans.tsv" "${actual}/plans-normalized.tsv"

  if grep -Fq 'frame= 1 fps=' "${actual}/plans-normalized.tsv"; then
    echo "FAIL: ffmpeg progress line was not stripped" >&2
    fail=1
  fi
  if ! grep -Fq 'CUDA_ERROR_OUT_OF_MEMORY\nConversion failed' "${actual}/plans-normalized.tsv"; then
    echo "FAIL: non-progress stderr lines were not preserved" >&2
    fail=1
  fi

  rm -R "${actual}"
  trap - EXIT
}

run_ffmpeg_stderr_normalize_test
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: FAIL because `lib/ffmpeg-stderr-normalize.py` does not exist.

- [ ] **Step 3: Implement `ffmpeg-stderr-normalize.py`**

Create `scripts/e2e-policy-audit/lib/ffmpeg-stderr-normalize.py`:

```python
#!/usr/bin/env python3
"""Strip ffmpeg carriage-return progress spam from plan result stderr_tail."""

from __future__ import annotations

import argparse
import csv
import json
import re
from pathlib import Path
from typing import Any

PROGRESS_RE = re.compile(r"^\s*frame=\s*\d+\s+fps=")


def strip_progress(value: str) -> str:
    parts = re.split(r"\r|\n", value)
    kept = [part for part in parts if part.strip() and not PROGRESS_RE.match(part)]
    return "\n".join(kept)


def normalize_result(raw: str) -> str:
    try:
        parsed: Any = json.loads(raw)
    except json.JSONDecodeError:
        return raw
    if not isinstance(parsed, dict):
        return raw
    detail = parsed.get("detail")
    if isinstance(detail, dict) and isinstance(detail.get("stderr_tail"), str):
        detail["stderr_tail"] = strip_progress(detail["stderr_tail"])
        return json.dumps(parsed, separators=(",", ":"), sort_keys=True)
    return raw


def normalize_tsv(src: Path, dst: Path) -> None:
    with src.open(newline="") as in_file, dst.open("w", newline="") as out_file:
        reader = csv.DictReader(in_file, delimiter="\t")
        if reader.fieldnames is None:
            raise SystemExit(f"{src}: missing TSV header")
        writer = csv.DictWriter(
            out_file,
            fieldnames=reader.fieldnames,
            delimiter="\t",
            lineterminator="\n",
        )
        writer.writeheader()
        for row in reader:
            if "result" in row:
                row["result"] = normalize_result(row["result"])
            writer.writerow(row)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("input_tsv")
    parser.add_argument("output_tsv")
    args = parser.parse_args()
    normalize_tsv(Path(args.input_tsv), Path(args.output_tsv))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 4: Make it executable and rerun tests**

Run:

```bash
chmod +x scripts/e2e-policy-audit/lib/ffmpeg-stderr-normalize.py
scripts/e2e-policy-audit/tests/test.sh
```

Expected: `run_ffmpeg_stderr_normalize_test` passes.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-policy-audit/lib/ffmpeg-stderr-normalize.py scripts/e2e-policy-audit/tests/test.sh
git commit -m "scripts: normalize ffmpeg stderr tails for e2e diffs"
```

## Task 3: Failure Timeline

**Files:**
- Create: `scripts/e2e-policy-audit/lib/failure-timeline.py`
- Modify: `scripts/e2e-policy-audit/tests/test.sh`
- Create: `scripts/e2e-policy-audit/tests/expected/failure-timeline.md`

- [ ] **Step 1: Add the failing test**

Add this function to `scripts/e2e-policy-audit/tests/test.sh` after the ffmpeg normalizer test:

```bash
run_failure_timeline_test() {
  local actual
  actual=$(mktemp -d)
  trap 'rm -R "${actual}"' EXIT

  cat >"${actual}/plans.tsv" <<'EOF'
id	file_id	phase_name	status	result	executed_at
plan-1	file-1	transcode-video	failed	{"detail":{"stderr_tail":"CUDA_ERROR_OUT_OF_MEMORY"},"error":"ffmpeg exited with exit status: 187"}	2026-05-14T10:01:00Z
plan-2	file-2	transcode-video	failed	{"detail":{"stderr_tail":"CUDA_ERROR_OUT_OF_MEMORY"},"error":"ffmpeg exited with exit status: 187"}	2026-05-14T10:15:00Z
plan-3	file-3	transcode-video	failed	{"detail":{"stderr_tail":"No device available for decoder"},"error":"ffmpeg exited with exit status: 1"}	2026-05-14T11:00:00Z
plan-4	file-4	transcode-video	failed	{"detail":{"stderr_tail":"Impossible to convert between the formats"},"error":"ffmpeg exited with exit status: 1"}	2026-05-14T11:05:00Z
plan-5	file-5	transcode-video	failed	{"detail":{"stderr_tail":"unclassified failure"},"error":"executor failed"}	2026-05-14T11:10:00Z
plan-6	file-6	transcode-video	completed	{"ok":true}	2026-05-14T11:15:00Z
EOF

  "lib/failure-timeline.py" "${actual}/plans.tsv" "${actual}/failure-timeline.md"
  assert_match "${actual}/failure-timeline.md" "tests/expected/failure-timeline.md"

  rm -R "${actual}"
  trap - EXIT
}

run_failure_timeline_test
```

Create `scripts/e2e-policy-audit/tests/expected/failure-timeline.md`:

```markdown
# Failure Timeline

| Hour | cuda-no-device | cuda-oom | filter-format | other | Total |
|---|---:|---:|---:|---:|---:|
| 2026-05-14T10:00:00Z | 0 | 2 | 0 | 0 | 2 |
| 2026-05-14T11:00:00Z | 1 | 0 | 1 | 1 | 3 |
```

- [ ] **Step 2: Run the test and verify it fails**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
```

Expected: FAIL because `lib/failure-timeline.py` does not exist.

- [ ] **Step 3: Implement `failure-timeline.py`**

Create `scripts/e2e-policy-audit/lib/failure-timeline.py`:

```python
#!/usr/bin/env python3
"""Bucket failed plans by hour and primary failure signature."""

from __future__ import annotations

import argparse
import csv
import json
from collections import OrderedDict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

COLUMNS = ["cuda-no-device", "cuda-oom", "filter-format", "other"]


def text_for_result(raw: str) -> str:
    try:
        parsed: Any = json.loads(raw)
    except json.JSONDecodeError:
        return raw
    if not isinstance(parsed, dict):
        return raw
    detail = parsed.get("detail") or {}
    parts = [
        str(parsed.get("error") or ""),
        str(detail.get("stderr_tail") or ""),
        str(detail.get("command") or ""),
    ]
    return "\n".join(parts)


def classify(text: str) -> str:
    if "No device available for decoder" in text or "CUDA_ERROR_NO_DEVICE" in text:
        return "cuda-no-device"
    if "CUDA_ERROR_OUT_OF_MEMORY" in text or "out of memory" in text.lower():
        return "cuda-oom"
    if "Impossible to convert between the formats" in text:
        return "filter-format"
    return "other"


def hour_bucket(raw: str) -> str:
    raw = raw.strip()
    if not raw:
        return "unknown"
    normalized = raw.replace("Z", "+00:00")
    try:
        dt = datetime.fromisoformat(normalized)
    except ValueError:
        return raw[:13] + ":00:00Z" if len(raw) >= 13 else "unknown"
    if dt.tzinfo is not None:
        dt = dt.astimezone(timezone.utc)
    return dt.replace(minute=0, second=0, microsecond=0).strftime("%Y-%m-%dT%H:00:00Z")


def build_timeline(plans_tsv: Path) -> OrderedDict[str, dict[str, int]]:
    buckets: OrderedDict[str, dict[str, int]] = OrderedDict()
    with plans_tsv.open(newline="") as f:
        reader = csv.DictReader(f, delimiter="\t")
        if reader.fieldnames is None:
            raise SystemExit(f"{plans_tsv}: missing TSV header")
        required = {"status", "result", "executed_at"}
        missing = sorted(required.difference(reader.fieldnames))
        if missing:
            raise SystemExit(f"{plans_tsv}: missing required column(s): {', '.join(missing)}")
        for row in reader:
            if row["status"] != "failed":
                continue
            hour = hour_bucket(row["executed_at"])
            signature = classify(text_for_result(row["result"]))
            if hour not in buckets:
                buckets[hour] = {column: 0 for column in COLUMNS}
            buckets[hour][signature] += 1
    return buckets


def write_markdown(buckets: OrderedDict[str, dict[str, int]], out_md: Path) -> None:
    out_md.parent.mkdir(parents=True, exist_ok=True)
    with out_md.open("w") as f:
        f.write("# Failure Timeline\n\n")
        if not buckets:
            f.write("(none)\n")
            return
        f.write("| Hour | cuda-no-device | cuda-oom | filter-format | other | Total |\n")
        f.write("|---|---:|---:|---:|---:|---:|\n")
        for hour in sorted(buckets):
            counts = buckets[hour]
            total = sum(counts.values())
            f.write(
                f"| {hour} | {counts['cuda-no-device']} | {counts['cuda-oom']} | "
                f"{counts['filter-format']} | {counts['other']} | {total} |\n"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("plans_tsv")
    parser.add_argument("out_md")
    args = parser.parse_args()
    write_markdown(build_timeline(Path(args.plans_tsv)), Path(args.out_md))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 4: Make it executable and rerun tests**

Run:

```bash
chmod +x scripts/e2e-policy-audit/lib/failure-timeline.py
scripts/e2e-policy-audit/tests/test.sh
```

Expected: `run_failure_timeline_test` passes.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-policy-audit/lib/failure-timeline.py \
  scripts/e2e-policy-audit/tests/test.sh \
  scripts/e2e-policy-audit/tests/expected/failure-timeline.md
git commit -m "scripts: add e2e failure timeline"
```

## Task 4: Wire New Artifacts Into The Harness

**Files:**
- Modify: `scripts/e2e-policy-audit/run.sh`
- Modify: `scripts/e2e-policy-audit/lib/build-summary.sh`

- [ ] **Step 1: Update run directory creation**

In `scripts/e2e-policy-audit/run.sh`, change:

```bash
mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke,diffs,runtime} "${run_dir}/logs/env-check"
```

to:

```bash
mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke,diffs,runtime} \
    "${run_dir}/logs/env-check" \
    "${run_dir}/logs/plugin-errors"
```

- [ ] **Step 2: Run plugin-error dedupe after event capture**

In `scripts/e2e-policy-audit/run.sh`, immediately after:

```bash
log_run events "${voom_bin}" events -n 1000000 -f json
cp "${run_dir}/logs/events.log" "${run_dir}/reports/events.json"
```

add:

```bash
"${lib_dir}/plugin-error-dedupe.py" \
    "${run_dir}/reports/events.json" \
    "${run_dir}/reports/events-deduped.json" \
    "${run_dir}/logs/plugin-errors" \
    "${run_dir}/diffs/plugin-error-summary.md" ||
    echo "plugin error dedupe failed (continuing)" >&2
```

- [ ] **Step 3: Normalize exported plan stderr before summary/diffs**

In `scripts/e2e-policy-audit/run.sh`, after copying post-run DB tables into `db-export/`:

```bash
cp -r "${run_dir}/post/voom-db-tables/." "${run_dir}/db-export/"
```

add:

```bash
if [[ -f "${run_dir}/db-export/plans.tsv" ]]; then
    "${lib_dir}/ffmpeg-stderr-normalize.py" \
        "${run_dir}/db-export/plans.tsv" \
        "${run_dir}/db-export/plans.normalized.tsv" &&
        mv "${run_dir}/db-export/plans.normalized.tsv" "${run_dir}/db-export/plans.tsv" ||
        echo "ffmpeg stderr normalization failed (continuing)" >&2
fi
```

- [ ] **Step 4: Generate the failure timeline during diff generation**

In `scripts/e2e-policy-audit/run.sh`, after the env-check timeline block and before `stage_end diff "$t"`, add:

```bash
if [[ -f "${run_dir}/db-export/plans.tsv" ]]; then
    "${lib_dir}/failure-timeline.py" \
        "${run_dir}/db-export/plans.tsv" \
        "${run_dir}/diffs/failure-timeline.md" ||
        echo "failure timeline generation failed (continuing)" >&2
fi
```

- [ ] **Step 5: Preview new artifacts in the summary**

In `scripts/e2e-policy-audit/lib/build-summary.sh`, after the `### Failure clusters` block, add:

```bash
  echo
  echo "### Failure timeline"
  if [[ -s "${run}/diffs/failure-timeline.md" ]]; then
    sed -n '1,24p' "${run}/diffs/failure-timeline.md"
  else
    echo "(not generated)"
  fi
  echo
  echo "### Plugin error summary"
  if [[ -s "${run}/diffs/plugin-error-summary.md" ]]; then
    sed -n '1,24p' "${run}/diffs/plugin-error-summary.md"
  else
    echo "(not generated)"
  fi
```

Near the existing linked artifacts section in the same file, add:

```bash
  link_artifact_if_exists "diffs/failure-timeline.md"
  link_artifact_if_exists "diffs/plugin-error-summary.md"
  link_artifact_if_exists "reports/events-deduped.json"
  link_artifact_if_exists "logs/plugin-errors/"
```

- [ ] **Step 6: Run shell syntax checks and tests**

Run:

```bash
bash -n scripts/e2e-policy-audit/run.sh
bash -n scripts/e2e-policy-audit/lib/build-summary.sh
scripts/e2e-policy-audit/tests/test.sh
```

Expected: both `bash -n` commands exit 0; test script passes.

- [ ] **Step 7: Commit**

```bash
git add scripts/e2e-policy-audit/run.sh scripts/e2e-policy-audit/lib/build-summary.sh
git commit -m "scripts: wire e2e signal artifacts"
```

## Task 5: Web Smoke Content Assertions

**Files:**
- Modify: `scripts/e2e-policy-audit/lib/web-smoke.sh`
- Modify: `scripts/e2e-policy-audit/tests/test.sh`

- [ ] **Step 1: Add a focused validation-helper test**

Add this function to `scripts/e2e-policy-audit/tests/test.sh` near other focused tests:

```bash
run_web_smoke_validation_helpers_test() {
  local actual
  actual=$(mktemp -d)
  trap 'rm -R "${actual}"' EXIT

  cat >"${actual}/files.body" <<'EOF'
{"files":[{"id":"file-1","path":"/lib/a.mkv","tracks":[]}],"total":1}
EOF
  cat >"${actual}/jobs.body" <<'EOF'
{"jobs":[{"id":"job-1","status":"failed"}],"total":1}
EOF
  cat >"${actual}/events.body" <<'EOF'
event: job-update
data: {"JobProgress":{"job_id":"job-1","progress":0.5,"message":null}}

EOF
  cat >"${actual}/root.body" <<'EOF'
<!doctype html><html><head><title>VOOM</title></head><body>VOOM</body></html>
EOF

  WEB_SMOKE_TEST_MODE=1 source "lib/web-smoke.sh"
  validate_root_body "${actual}/root.body"
  validate_files_body "${actual}/files.body"
  validate_jobs_body "${actual}/jobs.body" 1
  validate_sse_body "${actual}/events.body"

  rm -R "${actual}"
  trap - EXIT
}

run_web_smoke_validation_helpers_test
```

- [ ] **Step 2: Refactor `web-smoke.sh` to expose validation helpers**

Replace `scripts/e2e-policy-audit/lib/web-smoke.sh` with this content:

```bash
#!/usr/bin/env bash
# Starts voom serve, hits endpoint smoke checks, captures statuses + body
# samples to <out-dir>, then shuts the server down.
# Usage: web-smoke.sh <voom-bin> <out-dir> [db-path]
set -euo pipefail

validate_root_body() {
    local body_path="$1"
    grep -Eq '<title>VOOM|VOOM' "${body_path}"
}

validate_files_body() {
    local body_path="$1"
    jq -e '
      (.files | type == "array") and
      (.files | length >= 1) and
      (.files[0].id | type == "string") and
      (.files[0].path | type == "string") and
      (.total | type == "number")
    ' "${body_path}" >/dev/null
}

validate_jobs_body() {
    local body_path="$1"
    local expected_failed="$2"
    jq -e --argjson expected "${expected_failed}" '
      (.jobs | type == "array") and
      (.total == $expected) and
      (all(.jobs[]; .status == "failed"))
    ' "${body_path}" >/dev/null
}

validate_sse_body() {
    local body_path="$1"
    awk '
      /^event: / {event_seen=1}
      /^data: / {
        data_seen=1
        data=substr($0, 7)
        cmd="jq -e . >/dev/null"
        print data | cmd
        close(cmd)
        if (cmd == 0) valid_json=1
      }
      END {exit ! (event_seen && data_seen && valid_json)}
    ' "${body_path}"
}

failed_job_count_from_db() {
    local db_path="$1"
    if [[ -z "${db_path}" || ! -f "${db_path}" ]]; then
        printf '0\n'
        return 0
    fi
    sqlite3 "${db_path}" "SELECT COUNT(*) FROM jobs WHERE status = 'failed';"
}

if [[ "${WEB_SMOKE_TEST_MODE:-0}" == "1" ]]; then
    return 0
fi

voom_bin="${1:?voom binary path required}"
out_dir="${2:?output dir required}"
db_path="${3:-${HOME}/.config/voom/voom.db}"

mkdir -p "${out_dir}"
port="${WEB_SMOKE_PORT:-18080}"
log="${out_dir}/serve.log"

"${voom_bin}" serve --port "${port}" >"${log}" 2>&1 &
serve_pid=$!
trap 'kill "${serve_pid}" 2>/dev/null || true; wait "${serve_pid}" 2>/dev/null || true' EXIT

for _ in {1..20}; do
    if ! kill -0 "${serve_pid}" 2>/dev/null; then
        echo "web-smoke: voom serve died before binding (see ${log})" >&2
        exit 1
    fi
    if curl -fsS "http://127.0.0.1:${port}/" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

statuses="${out_dir}/statuses.tsv"
printf 'endpoint\tstatus\tcontent\n' >"${statuses}"

probe() {
    local label="$1"
    local url="$2"
    local body_path="${out_dir}/${label}.body"
    local status
    status=$(curl -s -o "${body_path}" -w '%{http_code}' --max-time 8 "${url}" || true)
    [[ -z "${status}" ]] && status="000"
    printf '%s\n' "${status}"
}

record_probe() {
    local label="$1"
    local url="$2"
    local validator="$3"
    local expected="${4:-}"
    local status
    local content="FAIL"
    status=$(probe "${label}" "${url}")
    if [[ "${status}" =~ ^2[0-9][0-9]$ ]] && "${validator}" "${out_dir}/${label}.body" "${expected}"; then
        content="PASS"
    fi
    printf '%s\t%s\t%s\n' "${label}" "${status}" "${content}" >>"${statuses}"
}

record_probe root "http://127.0.0.1:${port}/" validate_root_body
record_probe api-files "http://127.0.0.1:${port}/api/files?limit=1" validate_files_body
failed_count="$(failed_job_count_from_db "${db_path}")"
record_probe api-jobs-failed "http://127.0.0.1:${port}/api/jobs?status=failed" \
    validate_jobs_body "${failed_count}"

sse_body="${out_dir}/events-sse.body"
sse_status=$(curl -s -N -o "${sse_body}" -w '%{http_code}' --max-time 5 \
    "http://127.0.0.1:${port}/events" || true)
[[ -z "${sse_status}" ]] && sse_status="000"
sse_content="FAIL"
if [[ "${sse_status}" =~ ^2[0-9][0-9]$ ]] && validate_sse_body "${sse_body}"; then
    sse_content="PASS"
fi
printf '%s\t%s\t%s\n' "events(sse)" "${sse_status}" "${sse_content}" >>"${statuses}"

cat "${statuses}"
awk -F '\t' 'NR > 1 && ($2 !~ /^2/ || $3 != "PASS") {bad=1} END {exit bad}' "${statuses}"
```

- [ ] **Step 3: Update summary interpretation for content column**

In `scripts/e2e-policy-audit/lib/build-summary.sh`, replace the web smoke loop with:

```bash
if [[ -f "${statuses}" ]]; then
  while IFS=$'\t' read -r ep st content; do
    [[ "${ep}" == "endpoint" ]] && continue
    if [[ "${ep}" == *"(sse)"* ]]; then
      [[ ! "${st}" =~ ^2[0-9][0-9]$ ]] && note_warn "SSE smoke ${ep} returned ${st}"
      [[ "${content:-PASS}" != "PASS" ]] && note_warn "SSE smoke ${ep} content assertion failed"
      continue
    fi
    [[ ! "${st}" =~ ^2[0-9][0-9]$ ]] && note_fail "web smoke ${ep} returned ${st}"
    [[ "${content:-PASS}" != "PASS" ]] && note_fail "web smoke ${ep} content assertion failed"
  done <"${statuses}"
fi
```

- [ ] **Step 4: Pass the DB path from `run.sh`**

In `scripts/e2e-policy-audit/run.sh`, change:

```bash
"${lib_dir}/web-smoke.sh" "${voom_bin}" "${run_dir}/web-smoke" \
```

to:

```bash
"${lib_dir}/web-smoke.sh" "${voom_bin}" "${run_dir}/web-smoke" "${db_path}" \
```

- [ ] **Step 5: Run tests**

Run:

```bash
bash -n scripts/e2e-policy-audit/lib/web-smoke.sh
bash -n scripts/e2e-policy-audit/lib/build-summary.sh
scripts/e2e-policy-audit/tests/test.sh
```

Expected: shell syntax checks exit 0; `run_web_smoke_validation_helpers_test` passes.

- [ ] **Step 6: Commit**

```bash
git add scripts/e2e-policy-audit/lib/web-smoke.sh \
  scripts/e2e-policy-audit/lib/build-summary.sh \
  scripts/e2e-policy-audit/run.sh \
  scripts/e2e-policy-audit/tests/test.sh
git commit -m "scripts: strengthen e2e web smoke assertions"
```

## Task 6: Documentation And Final Verification

**Files:**
- Modify: `scripts/e2e-policy-audit/README.md`

- [ ] **Step 1: Document new artifacts in the run-dir layout**

In `scripts/e2e-policy-audit/README.md`, update the layout section so `logs/`, `reports/`, `web-smoke/`, and `diffs/` include:

```markdown
├── logs/
│   ├── plugin-errors/            compact repeated plugin.error signature logs
│   └── env-check/                hourly voom env check snapshots during process
├── reports/
│   ├── events.json               raw `voom events -f json` capture
│   └── events-deduped.json       raw events with repeated plugin errors compacted
├── web-smoke/                    statuses + body samples + content assertions
├── diffs/
│   ├── plugin-error-summary.md   repeated plugin.error signatures by plugin
│   ├── failure-timeline.md       failed plans bucketed by hour and cause
```

- [ ] **Step 2: Document the recommended inspection order**

In the “Interpreting `summary.md`” section, replace the aggregate views bullet list with:

```markdown
For large runs, start with the aggregate views:

- `diffs/failure-timeline.md` shows whether failures are clustered in time or
  spread across the run.
- `diffs/failure-clusters.md` groups failed plans by phase, error signature,
  exit code, source container, and source video codec.
- `diffs/plugin-error-summary.md` compresses repeated plugin error payloads and
  points to per-plugin logs under `logs/plugin-errors/`.
- `diffs/db-vs-ffprobe-post-summary.md` groups post-run introspection
  divergences by stable signatures such as subtitle default drift or attachment
  promotion.
- `diffs/ffprobe-pre-vs-post-summary.md` groups actual on-disk metadata changes
  independently of VOOM's DB view.
- `repro/minimal-covering-set.tsv` picks a capped set of representative files
  per failure/diff signature for faster follow-up runs.
```

- [ ] **Step 3: Run final verification**

Run:

```bash
scripts/e2e-policy-audit/tests/test.sh
bash -n scripts/e2e-policy-audit/run.sh
bash -n scripts/e2e-policy-audit/lib/web-smoke.sh
bash -n scripts/e2e-policy-audit/lib/build-summary.sh
```

Expected: all commands exit 0.

- [ ] **Step 4: Review changed files**

Run:

```bash
git diff --stat
git diff -- docs/superpowers/plans/2026-05-14-issue-400-e2e-signal-noise.md scripts/e2e-policy-audit
```

Expected: diffs are limited to this plan and `scripts/e2e-policy-audit` script/test/docs changes.

- [ ] **Step 5: Commit**

```bash
git add scripts/e2e-policy-audit/README.md
git commit -m "docs: describe e2e signal artifacts"
```

## Self-Review

Spec coverage:
- Demux noisy plugin output: Task 1 creates post-process dedupe, `events-deduped.json`, `plugin-error-summary.md`, and per-plugin logs. Task 4 wires it into the harness.
- Strip ffmpeg progress spam: Task 2 normalizes `stderr_tail` before summary and diff artifacts consume `plans.tsv`.
- Failure timeline bucket: Task 3 derives `diffs/failure-timeline.md` from `plans.tsv` using `executed_at` and classified result text.
- Web smoke beyond HTTP 200: Task 5 adds shape/content assertions for root, files, failed jobs, and SSE.
- Script-only changes: all implementation work stays under `scripts/e2e-policy-audit`.

Placeholder scan:
- No task relies on unspecified behavior. Each new script has complete code, each test has concrete fixture data, and each integration point names exact insertion locations.

Type and field consistency:
- `plans.tsv` fields match the existing `plan-phase-summary.py` contract plus the issue-required `executed_at` field.
- Web API assertions match current response objects in `plugins/web-server/src/api/files.rs` and `plugins/web-server/src/api/jobs.rs`: `files`, `jobs`, and `total`.
- Event dedupe consumes the current `voom events -f json` JSON array shape from `crates/voom-cli/src/commands/events.rs`.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-14-issue-400-e2e-signal-noise.md`. Two execution options:

**1. Subagent-Driven (recommended)** - Dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.
