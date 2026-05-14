#!/usr/bin/env python3
import argparse
import re
from pathlib import Path


LINE_RE = re.compile(r"^\s*([^:]+)\s*:\s*(.*?)\s*$")
SAMPLE_LOG_RE = re.compile(r"^\d{4}\.log$")
GPU_DEVICE_RE = re.compile(r"^\s*GPU \d+:", re.IGNORECASE)
TOOL_RE = re.compile(
    r"^\s*(ffmpeg|rclone)\b.*?\b(OK|FAIL|not found)\b",
    re.IGNORECASE,
)
BACKEND_RE = re.compile(r"^\s*Backend\b.*$", re.IGNORECASE)
NVENC_UNAVAILABLE_RE = re.compile(
    r"\b(none|unavailable|disabled|not found|fail|failed)\b",
    re.IGNORECASE,
)
NAME_MAP = {
    "nvenc": "nvenc",
    "gpu": "gpus",
    "gpus": "gpus",
    "ffmpeg": "ffmpeg",
    "rclone": "rclone",
}


def normalize_name(name: str) -> str | None:
    return NAME_MAP.get(name.strip().lower())


def normalize_status(value: str) -> str:
    return value.strip().upper() or "UNKNOWN"


def parse_synthetic_line(line: str, values: dict) -> bool:
    match = LINE_RE.match(line)
    if not match:
        return False

    name = normalize_name(match.group(1))
    if name is None:
        return False

    value = match.group(2)
    if name == "gpus":
        gpu_match = re.search(r"\d+", value)
        if gpu_match:
            values[name] = int(gpu_match.group(0))
            values["_synthetic_gpus"] = True
    else:
        values[name] = normalize_status(value)
    return True


def parse_tool_line(line: str, values: dict) -> bool:
    match = TOOL_RE.match(line)
    if not match:
        return False

    tool = match.group(1).lower()
    values[tool] = normalize_status(match.group(2))
    return True


def parse_backend_line(line: str, values: dict) -> bool:
    if not BACKEND_RE.match(line):
        return False

    if re.search(r"\bNVENC\b", line, re.IGNORECASE):
        values["nvenc"] = "OK"
        return True

    if NVENC_UNAVAILABLE_RE.search(line):
        values["nvenc"] = "FAIL"
        return True

    return True


def parse_sample(path: Path) -> dict:
    values = {
        "nvenc": "UNKNOWN",
        "gpus": 0,
        "ffmpeg": "UNKNOWN",
        "rclone": "UNKNOWN",
        "_synthetic_gpus": False,
    }
    for line in path.read_text(errors="replace").splitlines():
        if parse_synthetic_line(line, values):
            continue
        if parse_tool_line(line, values):
            continue
        parse_backend_line(line, values)

        if not values["_synthetic_gpus"] and GPU_DEVICE_RE.match(line):
            values["gpus"] += 1

    if values["nvenc"] == "UNKNOWN" and values["gpus"] == 0:
        values["nvenc"] = "FAIL"

    return {
        "nvenc": values["nvenc"],
        "gpus": values["gpus"],
        "ffmpeg": values["ffmpeg"],
        "rclone": values["rclone"],
    }


def render_status(sample: dict) -> str:
    return f"NVENC {sample['nvenc']}"


def state_transitions(samples: list[dict]) -> list[str]:
    transitions = []
    for index, sample in enumerate(samples):
        if index == 0:
            continue

        previous = samples[index - 1]
        changes = []
        if sample["nvenc"] != previous["nvenc"]:
            changes.append(f"NVENC {previous['nvenc']} -> {sample['nvenc']}")
        if sample["gpus"] != previous["gpus"]:
            changes.append(f"GPUs {previous['gpus']} -> {sample['gpus']}")
        if sample["ffmpeg"] != previous["ffmpeg"]:
            changes.append(f"ffmpeg {previous['ffmpeg']} -> {sample['ffmpeg']}")
        if sample["rclone"] != previous["rclone"]:
            changes.append(f"rclone {previous['rclone']} -> {sample['rclone']}")

        if changes:
            transitions.append(f"sample {index + 1}: {'; '.join(changes)}")
    return transitions


def render(samples: list[dict], transitions: list[str]) -> str:
    lines = [
        "# Env Check Timeline",
        "",
        f"Samples: {len(samples)}",
        "",
        "| sample | status | gpus | ffmpeg | rclone |",
        "|---:|---|---:|---|---|",
    ]
    for index, sample in enumerate(samples, start=1):
        lines.append(
            f"| {index} | {render_status(sample)} | {sample['gpus']} | "
            f"{sample['ffmpeg']} | {sample['rclone']} |"
        )

    lines.extend(["", "## State Transitions", ""])
    if transitions:
        lines.extend(f"- {transition}" for transition in transitions)
    else:
        lines.append("- None")

    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarize env check sampler logs.")
    parser.add_argument("input_dir")
    parser.add_argument("output")
    args = parser.parse_args()

    input_dir = Path(args.input_dir)
    if not input_dir.is_dir():
        parser.error(f"env check log dir not found: {input_dir}")

    samples = [
        parse_sample(path)
        for path in sorted(input_dir.iterdir())
        if SAMPLE_LOG_RE.match(path.name)
        if path.is_file()
    ]
    transitions = state_transitions(samples)

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(render(samples, transitions))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
