#!/usr/bin/env bash
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

mapfile -t paths < <(awk -F '\t' 'NR > 1 && $1 != "" {print $1}' "${paths_file}")
if ((${#paths[@]} == 0)); then
  echo "No failed plan files to replay."
  exit 0
fi

exec "${BUILD}" process -y --on-error continue --policy "${POLICY}" "${paths[@]}"
