#!/usr/bin/env bash
# Verifies the host is in a state suitable for an E2E run, and writes
# manifest.json at the run-dir root.
# Usage: preflight.sh <policy-path> <library-path> <run-dir> <voom-bin>
set -euo pipefail

policy_path="${1:?policy path required}"
library_path="${2:?library path required}"
run_dir="${3:?run dir required}"
voom_bin="${4:?voom binary path required}"

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

required_tools=(ffmpeg ffprobe mkvmerge sqlite3 jq curl find xargs awk python3)
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
if [[ ! -d "${library_path}" ]]; then
    echo "PREFLIGHT FAIL: library not a directory: ${library_path}" >&2
    exit 1
fi

policy_sha=$(sha256sum "${policy_path}" | awk '{print $1}')
voom_version=$("${voom_bin}" --version 2>/dev/null || echo "unknown")
git_sha=$(git rev-parse HEAD 2>/dev/null || echo "unknown")
kernel=$(uname -sr)
cpu=$(awk -F': ' '/^model name/ {print $2; exit}' /proc/cpuinfo 2>/dev/null || echo "unknown")
gpu=$(command -v nvidia-smi >/dev/null && nvidia-smi --query-gpu=name --format=csv,noheader,nounits 2>/dev/null | head -1 || echo "none")

jq -n \
    --arg started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg voom_version "${voom_version}" \
    --arg git_sha "${git_sha}" \
    --arg voom_binary "${voom_bin}" \
    --arg policy_path "${policy_path}" \
    --arg policy_sha256 "${policy_sha}" \
    --arg library "${library_path}" \
    --arg kernel "${kernel}" \
    --arg cpu "${cpu}" \
    --arg gpu "${gpu}" \
    '{
        started_at: $started_at,
        completed_at: null,
        voom: { version: $voom_version, git_sha: $git_sha, binary: $voom_binary },
        policy: { path: $policy_path, sha256: $policy_sha256 },
        library: $library,
        host: { kernel: $kernel, cpu: $cpu, gpu: $gpu },
        stages: {}
    }' >"${run_dir}/manifest.json"

echo "preflight OK"
