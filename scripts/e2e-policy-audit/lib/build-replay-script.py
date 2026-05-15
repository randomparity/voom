#!/usr/bin/env python3
"""Generate repro/replay.sh for failed e2e plan files."""

from __future__ import annotations

import argparse
import json
from pathlib import Path


SCRIPT = """#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
run_dir="$(cd "${script_dir}/.." && pwd)"

BUILD="${BUILD:-$(command -v voom)}"
POLICY="${POLICY:-${run_dir}/env/policy.voom}"
paths_file="${script_dir}/failed-plan-files.tsv"

if [[ -z "${BUILD}" || ! -x "${BUILD}" ]]; then
  echo "voom binary not found; set BUILD=/path/to/voom" >&2
  exit 1
fi

if [[ ! -r "${paths_file}" ]]; then
  echo "failed plan list not found: ${paths_file}" >&2
  exit 1
fi

mapfile -t paths < <(awk -F '\\t' 'NR > 1 && $1 != "" {print $1}' "${paths_file}")
if ((${#paths[@]} == 0)); then
  echo "No failed plan files to replay."
  exit 0
fi

exec "${BUILD}" process -y --on-error continue --policy "${POLICY}" "${paths[@]}"
"""


def validate_json_if_present(path: Path) -> None:
    if path.exists():
        with path.open() as f:
            json.load(f)


def build(run_dir: Path) -> None:
    validate_json_if_present(run_dir / "manifest.json")
    validate_json_if_present(run_dir / "reports" / "process.json")

    repro_dir = run_dir / "repro"
    repro_dir.mkdir(parents=True, exist_ok=True)
    script = repro_dir / "replay.sh"
    script.write_text(SCRIPT)
    script.chmod(0o755)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("run_dir")
    args = parser.parse_args()
    build(Path(args.run_dir))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
