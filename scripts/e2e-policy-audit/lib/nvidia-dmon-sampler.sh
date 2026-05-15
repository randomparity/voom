#!/usr/bin/env bash
# Captures nvidia-smi dmon output during voom process.
# Usage: nvidia-dmon-sampler.sh <run-dir> [interval-seconds]
set -euo pipefail

run_dir="${1:?run dir required}"
interval="${2:-30}"
out="${run_dir}/runtime/nvidia-dmon.csv"
mkdir -p "$(dirname "${out}")"

if ! command -v nvidia-smi >/dev/null 2>&1; then
    {
        printf '# nvidia-smi not found\n'
        printf '# timestamp: %s\n' "$(date -Iseconds)"
    } >"${out}"
    exit 0
fi

exec nvidia-smi dmon -s pcu -d "${interval}" -o DT >"${out}"
