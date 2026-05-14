#!/usr/bin/env bash
# Capture host journal and package state after an e2e policy audit run.
# Usage: capture-host-journal.sh <run-dir> <start-ts>
set -euo pipefail

run_dir="${1:?run dir required}"
START_TS="${2:?start timestamp required}"
START_DATE="${START_TS%%T*}"
out_dir="${run_dir}/env"

mkdir -p "${out_dir}"

capture_command() {
    local out="$1"
    shift

    {
        printf '$'
        printf ' %q' "$@"
        printf '\n\n'
        "$@" 2>&1 || printf '[command failed: rc=%s]\n' "$?"
    } >"${out}"
}

capture_command "${out_dir}/journal.log" journalctl --since "${START_TS}" -p warning --no-pager
capture_command "${out_dir}/dmesg.log" dmesg --time-format=iso
capture_command "${out_dir}/dnf-history.txt" dnf history list
capture_command "${out_dir}/rpm-recently-changed.txt" bash -c 'rpm -qa --last | head -100'
