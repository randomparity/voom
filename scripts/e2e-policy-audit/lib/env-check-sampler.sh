#!/usr/bin/env bash
# Periodically capture voom env check output while voom process is running.
# Usage: env-check-sampler.sh <run-dir> <voom-bin> [interval-seconds]
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    echo "usage: env-check-sampler.sh <run-dir> <voom-bin> [interval-seconds]" >&2
    exit 2
fi

run_dir="$1"
voom_bin="$2"
interval="${3:-3600}"
out_dir="${run_dir}/logs/env-check"
mkdir -p "${out_dir}"

sample_index=0
active_pid=""
sleep_pid=""

cleanup() {
    if [[ -n "${active_pid}" ]]; then
        kill "${active_pid}" 2>/dev/null || true
        wait "${active_pid}" 2>/dev/null || true
        active_pid=""
    fi
    if [[ -n "${sleep_pid}" ]]; then
        kill "${sleep_pid}" 2>/dev/null || true
        wait "${sleep_pid}" 2>/dev/null || true
        sleep_pid=""
    fi
}

terminate() {
    cleanup
    exit 0
}

trap terminate INT TERM
trap cleanup EXIT

while true; do
    sample_index=$((sample_index + 1))
    out="${out_dir}/$(printf '%04d.log' "${sample_index}")"
    {
        printf 'timestamp: %s\n\n' "$(date -Iseconds)"
        "${voom_bin}" env check 2>&1 &
        active_pid="$!"
        if wait "${active_pid}"; then
            rc=0
        else
            rc="$?"
        fi
        active_pid=""
        if [[ "${rc}" -ne 0 ]]; then
            printf '[env check failed: rc=%s]\n' "${rc}"
        fi
    } >"${out}"
    sleep "${interval}" &
    sleep_pid="$!"
    wait "${sleep_pid}"
    sleep_pid=""
done
