#!/usr/bin/env bash
# Verifies the host is in a state suitable for an E2E run.
# Usage: preflight.sh <policy-path>
set -euo pipefail

policy_path="${1:?policy path required}"

config_dir="${HOME}/.config/voom"
db_file="${config_dir}/voom.db"
plugins_dir="${config_dir}/plugins"

if [[ -e "${db_file}" ]]; then
    echo "PREFLIGHT FAIL: ${db_file} exists. Move it aside before running." >&2
    exit 1
fi
if [[ -e "${plugins_dir}" ]]; then
    echo "PREFLIGHT FAIL: ${plugins_dir} exists. Move it aside before running." >&2
    exit 1
fi

required_tools=(ffmpeg ffprobe mkvmerge sqlite3 jq curl find stat awk)
missing=()
for tool in "${required_tools[@]}"; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
        missing+=("${tool}")
    fi
done
if ((${#missing[@]} > 0)); then
    echo "PREFLIGHT FAIL: missing required tools: ${missing[*]}" >&2
    exit 1
fi

if [[ ! -r "${policy_path}" ]]; then
    echo "PREFLIGHT FAIL: policy not readable: ${policy_path}" >&2
    exit 1
fi

echo "preflight OK"
