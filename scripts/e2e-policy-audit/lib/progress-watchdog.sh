#!/usr/bin/env bash
# Watches voom jobs list for forward progress while voom process runs.
# Usage: progress-watchdog.sh <run-dir> <voom-bin> <target-pid> <target-uses-group:0|1> [interval] [stuck-polls]
set -euo pipefail

run_dir="${1:?run dir required}"
voom_bin="${2:?voom bin required}"
target_pid="${3:?target pid required}"
target_uses_group="${4:?target uses group required}"
interval="${5:-600}"
stuck_limit="${6:-12}"
log="${run_dir}/logs/watchdog.log"
rc_file="${run_dir}/logs/watchdog.log.rc"
mkdir -p "${run_dir}/logs"

last_signature=""
stuck_count=0
poll_count=0
sleep_pid=""

cleanup() {
    if [[ -n "${sleep_pid}" ]]; then
        kill "${sleep_pid}" 2>/dev/null || true
        wait "${sleep_pid}" 2>/dev/null || true
        sleep_pid=""
    fi
}

record_status() {
    local rc="$1"
    if [[ "${rc}" == "0" && -f "${rc_file}" && "$(cat "${rc_file}")" == "2" ]]; then
        return 0
    fi
    printf '%s\n' "${rc}" >"${rc_file}"
}

finish() {
    local rc="$1"
    record_status "${rc}"
    exit 0
}

target_is_live() {
    if ((target_uses_group)); then
        kill -0 "-${target_pid}" 2>/dev/null
    else
        kill -0 "${target_pid}" 2>/dev/null
    fi
}

signal_target() {
    kill -USR1 "${target_pid}" 2>/dev/null || true
}

trap 'cleanup; finish 0' INT TERM
trap cleanup EXIT

{
    printf 'started: %s\n' "$(date -Iseconds)"
    printf 'target_pid: %s\n' "${target_pid}"
    printf 'interval_seconds: %s\n' "${interval}"
    printf 'stuck_polls: %s\n' "${stuck_limit}"
} >>"${log}"

while target_is_live; do
    poll_count=$((poll_count + 1))
    sig=$("${voom_bin}" jobs list 2>/dev/null | sha256sum | awk '{print $1}' || true)
    if [[ -n "${last_signature}" && "${sig}" == "${last_signature}" ]]; then
        stuck_count=$((stuck_count + 1))
    else
        stuck_count=0
    fi
    last_signature="${sig}"

    printf '%s\tpoll=%s\tstuck=%s\tsignature=%s\n' \
        "$(date -Iseconds)" "${poll_count}" "${stuck_count}" "${sig}" >>"${log}"

    if ((stuck_count >= stuck_limit)); then
        printf 'WATCHDOG: no job-state change after %s unchanged poll(s)\n' "${stuck_count}" >>"${log}"
        record_status 2
        signal_target
        finish 2
    fi

    if [[ -n "${VOOM_E2E_WATCHDOG_TEST_MAX_POLLS:-}" ]] &&
        ((poll_count >= VOOM_E2E_WATCHDOG_TEST_MAX_POLLS)); then
        finish 0
    fi

    sleep "${interval}" &
    sleep_pid="$!"
    wait "${sleep_pid}"
    sleep_pid=""
done

finish 0
