#!/usr/bin/env bash
# Periodically capture host state while voom process is running.
# Usage: runtime-sampler.sh <run-dir> [interval-seconds]
set -euo pipefail

run_dir="${1:?run dir required}"
interval="${2:-300}"
out_dir="${run_dir}/runtime"
mkdir -p "${out_dir}"

sample_index=0
sleep_pid=""

cleanup() {
    if [[ -n "${sleep_pid}" ]]; then
        kill "${sleep_pid}" 2>/dev/null || true
        wait "${sleep_pid}" 2>/dev/null || true
        sleep_pid=""
    fi
}

trap cleanup INT TERM EXIT

capture_command() {
    local label="$1"
    shift
    printf '\n## %s\n\n' "${label}"
    printf '$'
    printf ' %q' "$@"
    printf '\n\n'
    "$@" 2>&1 || printf '[command failed: rc=%s]\n' "$?"
}

while true; do
    sample_index=$((sample_index + 1))
    ts="$(date -Iseconds)"
    out="${out_dir}/$(printf '%04d-%s.txt' "${sample_index}" "${ts//:/}")"
    {
        printf '# Runtime sample %04d\n\n' "${sample_index}"
        printf 'timestamp: %s\n\n' "${ts}"
        if command -v nvidia-smi >/dev/null 2>&1; then
            capture_command "nvidia-smi -q" nvidia-smi -q -d POWER,MEMORY,UTILIZATION,CLOCK
            capture_command "nvidia-smi -L" nvidia-smi -L
        else
            printf '\n## nvidia-smi\n\nnvidia-smi not found\n'
        fi
        capture_command "free -m" free -m
        capture_command "df -h" df -h /mnt/raid0 "${HOME}/.config/voom"
        capture_command "uptime" uptime
        capture_command "top rss processes" bash -lc 'ps -eo pid,user,%cpu,%mem,rss,cmd --sort=-rss | head -6'
        capture_command "voom jobs list tail" bash -lc 'voom jobs list 2>/dev/null | tail -20'
    } >"${out}"
    sleep "${interval}" &
    sleep_pid="$!"
    wait "${sleep_pid}"
    sleep_pid=""
done
