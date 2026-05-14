#!/usr/bin/env python3
import argparse
import re
from pathlib import Path


GPU_RE = re.compile(r"^GPU \d+:", re.MULTILINE)
TIMESTAMP_RE = re.compile(r"^timestamp:\s+(.+)$", re.MULTILINE)
DF_RE = re.compile(r"^\S+\s+\S+\s+\S+\s+\S+\s+(\d+)%\s+(\S+)$", re.MULTILINE)
JOB_STATUS_RE = re.compile(
    r"(?<![\w-])(running|pending|completed|failed|cancelled|skipped)(?![\w-])",
    re.IGNORECASE,
)
JOBS_TOTAL_RE = re.compile(r"\bShowing\s+\d+\s+of\s+(\d+)\s+jobs\b", re.IGNORECASE)


def jobs_section_lines(text: str) -> list[str]:
    in_jobs_section = False
    lines = []
    for line in text.splitlines():
        if line.startswith("## "):
            in_jobs_section = line.strip() == "## voom jobs list tail"
            continue
        if in_jobs_section:
            lines.append(line)
    return lines


def count_job_rows(text: str) -> int:
    jobs_lines = jobs_section_lines(text)
    for line in jobs_lines:
        match = JOBS_TOTAL_RE.search(line)
        if match:
            return int(match.group(1))

    count = 0
    for line in jobs_lines:
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith(("#", "$", "[")):
            continue
        if JOB_STATUS_RE.search(stripped):
            count += 1
    return count


def parse_sample(path: Path) -> dict:
    text = path.read_text(errors="replace")
    timestamp_match = TIMESTAMP_RE.search(text)
    disks = [(mount, int(used)) for used, mount in DF_RE.findall(text)]
    return {
        "timestamp": timestamp_match.group(1) if timestamp_match else path.stem,
        "gpus": len(GPU_RE.findall(text)),
        "disks": disks,
        "max_disk": max((used for _, used in disks), default=0),
        "job_rows": count_job_rows(text),
    }


def disk_crossings(previous: dict, current: dict, threshold: int) -> list[str]:
    previous_by_mount = {mount: used for mount, used in previous["disks"]}
    transitions = []
    for mount, used in current["disks"]:
        previous_used = previous_by_mount.get(mount, 0)
        if previous_used < threshold <= used:
            transitions.append(
                f"{current['timestamp']}: disk {mount} crossed {threshold}% used ({used}%)"
            )
    return transitions


def state_transitions(samples: list[dict], disk_threshold: int, stall_samples: int) -> list[str]:
    transitions = []
    job_streak = 1

    for index, sample in enumerate(samples):
        if index == 0:
            continue

        previous = samples[index - 1]
        if sample["gpus"] != previous["gpus"]:
            transitions.append(
                f"{sample['timestamp']}: GPU device count changed "
                f"{previous['gpus']} -> {sample['gpus']}"
            )

        transitions.extend(disk_crossings(previous, sample, disk_threshold))

        if sample["job_rows"] == previous["job_rows"]:
            job_streak += 1
        else:
            job_streak = 1

        if stall_samples > 0 and sample["job_rows"] > 0 and job_streak == stall_samples:
            transitions.append(
                f"{sample['timestamp']}: jobs list row count stalled at "
                f"{sample['job_rows']} for {stall_samples} samples"
            )

    return transitions


def render(samples: list[dict], transitions: list[str]) -> str:
    lines = [
        "# Runtime Timeline",
        "",
        f"Samples: {len(samples)}",
        "",
        "## State Transitions",
        "",
    ]

    if transitions:
        lines.extend(f"- {transition}" for transition in transitions)
    else:
        lines.append("- None")

    lines.extend(
        [
            "",
            "## Samples",
            "",
            "| sample | timestamp | gpus | max disk used | job rows |",
            "|---:|---|---:|---:|---:|",
        ]
    )
    for index, sample in enumerate(samples, start=1):
        lines.append(
            f"| {index} | {sample['timestamp']} | {sample['gpus']} | "
            f"{sample['max_disk']}% | {sample['job_rows']} |"
        )

    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarize runtime sampler snapshots.")
    parser.add_argument("runtime_dir")
    parser.add_argument("output")
    parser.add_argument("--disk-used-threshold", type=int, default=95)
    parser.add_argument("--job-stall-samples", type=int, default=3)
    args = parser.parse_args()

    runtime_dir = Path(args.runtime_dir)
    if not runtime_dir.is_dir():
        parser.error(f"runtime dir not found: {runtime_dir}")

    samples = [
        parse_sample(path)
        for path in sorted(runtime_dir.glob("*.txt"))
        if path.is_file()
    ]
    transitions = state_transitions(
        samples, args.disk_used_threshold, args.job_stall_samples
    )

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(render(samples, transitions))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
