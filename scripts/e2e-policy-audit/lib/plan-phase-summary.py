#!/usr/bin/env python3
"""Summarize plan outcomes by phase from a db-export plans.tsv file."""

from __future__ import annotations

import csv
import sys
from collections import OrderedDict
from pathlib import Path

REQUIRED_COLUMNS = {"id", "file_id", "phase_name", "status", "result"}
COUNTED_STATUSES = {"completed", "skipped", "failed"}


def usage() -> str:
    return "usage: plan-phase-summary.py <plans.tsv> <phase-summary.tsv> <failed-plans.tsv>"


def require_columns(fieldnames: list[str] | None, plans_path: Path) -> None:
    if fieldnames is None:
        raise SystemExit(f"{plans_path}: missing TSV header")

    missing = sorted(REQUIRED_COLUMNS.difference(fieldnames))
    if missing:
        joined = ", ".join(missing)
        raise SystemExit(f"{plans_path}: missing required column(s): {joined}")


def summarize(
    plans_path: Path,
    phase_summary_path: Path,
    failed_plans_path: Path,
) -> None:
    phases: OrderedDict[str, dict[str, int]] = OrderedDict()
    failed_plans: list[dict[str, str]] = []

    with plans_path.open(newline="") as plans_file:
        reader = csv.DictReader(plans_file, delimiter="\t")
        require_columns(reader.fieldnames, plans_path)

        for row in reader:
            phase = row["phase_name"] or "unknown"
            status = row["status"]
            if phase not in phases:
                phases[phase] = {"total": 0, "completed": 0, "skipped": 0, "failed": 0}

            phases[phase]["total"] += 1
            if status in COUNTED_STATUSES:
                phases[phase][status] += 1

            if status == "failed":
                failed_plans.append(
                    {
                        "plan_id": row["id"],
                        "file_id": row["file_id"],
                        "phase": phase,
                        "result": row["result"],
                    }
                )

    with phase_summary_path.open("w", newline="") as phase_summary_file:
        writer = csv.writer(phase_summary_file, delimiter="\t", lineterminator="\n")
        writer.writerow(["phase", "total", "completed", "skipped", "failed"])
        for phase, counts in phases.items():
            writer.writerow(
                [
                    phase,
                    counts["total"],
                    counts["completed"],
                    counts["skipped"],
                    counts["failed"],
                ]
            )

    with failed_plans_path.open("w") as failed_plans_file:
        failed_plans_file.write("plan_id\tfile_id\tphase\tresult\n")
        for failed_plan in failed_plans:
            failed_plans_file.write(
                "\t".join(
                    [
                        failed_plan["plan_id"],
                        failed_plan["file_id"],
                        failed_plan["phase"],
                        failed_plan["result"],
                    ]
                )
                + "\n"
            )


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(usage(), file=sys.stderr)
        return 2

    summarize(Path(argv[1]), Path(argv[2]), Path(argv[3]))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
