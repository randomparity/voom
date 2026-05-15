#!/usr/bin/env bash
# Periodically exports DB checkpoints and row-count growth while voom process runs.
# Usage: db-checkpoint-sampler.sh <run-dir> <db-path> [interval-seconds]
set -euo pipefail

run_dir="${1:?run dir required}"
db_path="${2:?db path required}"
interval="${3:-21600}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
growth="${run_dir}/diffs/db-growth.tsv"
checkpoint_root="${run_dir}/db-export"
tables=(files tracks jobs plans file_transitions bad_files discovered_files)

mkdir -p "${checkpoint_root}" "${run_dir}/diffs"

if [[ ! -f "${growth}" ]]; then
    printf 'checkpoint\ttable\trows\n' >"${growth}"
fi

sample_index=0
active_pid=""
active_uses_group=0
sleep_pid=""

cleanup() {
    if [[ -n "${active_pid}" ]]; then
        if ((active_uses_group)); then
            kill -- "-${active_pid}" 2>/dev/null || true
        else
            kill "${active_pid}" 2>/dev/null || true
        fi
        wait "${active_pid}" 2>/dev/null || true
        active_pid=""
        active_uses_group=0
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

start_active_command() {
    if command -v setsid >/dev/null 2>&1; then
        setsid "$@" &
        active_uses_group=1
    else
        "$@" &
        active_uses_group=0
    fi
    active_pid="$!"
}

write_growth_rows() {
    local checkpoint="$1"
    local db="$2"
    local table count count_file rc
    for table in "${tables[@]}"; do
        count_file=$(mktemp "${run_dir}/diffs/db-count.XXXXXX")
        start_active_command sqlite3 "${db}" "SELECT COUNT(*) FROM ${table};" >"${count_file}" 2>/dev/null
        if wait "${active_pid}"; then
            rc=0
        else
            rc="$?"
        fi
        active_pid=""
        active_uses_group=0
        if [[ "${rc}" -eq 0 ]]; then
            count=$(cat "${count_file}")
        else
            count=0
        fi
        rm -f "${count_file}"
        printf '%s\t%s\t%s\n' "${checkpoint}" "${table}" "${count}" >>"${growth}"
    done
}

while true; do
    sample_index=$((sample_index + 1))
    checkpoint="$(printf 'checkpoint-%04d' "${sample_index}")"
    out_dir="${checkpoint_root}/${checkpoint}"
    if [[ -r "${db_path}" ]]; then
        mkdir -p "${out_dir}"
        start_active_command "${script_dir}/db-export.sh" "${db_path}" "${out_dir}" >"${out_dir}/db-export.log" 2>&1
        if wait "${active_pid}"; then
            export_rc=0
        else
            export_rc="$?"
        fi
        active_pid=""
        active_uses_group=0
        if [[ "${export_rc}" -ne 0 ]]; then
            touch "${out_dir}/EXPORT_FAILED"
            printf '%s\t%s\t%s\n' "${checkpoint}" "__db_export_failed__" "0" >>"${growth}"
        fi
        write_growth_rows "${checkpoint}" "${db_path}"
    else
        printf '%s\t%s\t%s\n' "${checkpoint}" "__db_unreadable__" "0" >>"${growth}"
    fi

    if [[ "${VOOM_E2E_CHECKPOINT_TEST_ONCE:-0}" == "1" ]]; then
        exit 0
    fi

    sleep "${interval}" &
    sleep_pid="$!"
    wait "${sleep_pid}"
    sleep_pid=""
done
