#!/usr/bin/env bash
# Parallel ffprobe sweep of a library, mapping each file's ffprobe output
# to the canonical NDJSON schema. Output: <out-dir>/ffprobe.ndjson plus
# <out-dir>/ffprobe-failures.tsv for files ffprobe couldn't parse.
# Usage: probe.sh <library-root> <out-dir> [workers]
set -euo pipefail

lib_root="${1:?library root required}"
out_dir="${2:?output dir required}"
workers="${3:-8}"

if [[ ! -d "${lib_root}" ]]; then
    echo "probe: library root does not exist: ${lib_root}" >&2
    exit 1
fi
mkdir -p "${out_dir}"

manifest="${out_dir}/library-manifest.tsv"
if [[ ! -r "${manifest}" ]]; then
    echo "probe: library-manifest.tsv missing under ${out_dir}; run snapshot.sh first" >&2
    exit 1
fi

ndjson="${out_dir}/ffprobe.ndjson"
failures="${out_dir}/ffprobe-failures.tsv"
: >"${ndjson}"
: >"${failures}"

filter="$(dirname "$0")/ffprobe-to-ndjson.jq"
[[ -r "${filter}" ]] || {
    echo "probe: missing filter ${filter}" >&2
    exit 1
}

# Per-file worker. Receives a TSV line (path \t size \t mtime \t ext) on
# stdin via xargs -I{}; emits one NDJSON line on success or appends one
# row to ffprobe-failures.tsv on parse failure. Concurrent appends to
# ffprobe.ndjson rely on Linux's O_APPEND atomicity at the filesystem-block
# level (typically 4KB) — fine for typical NDJSON lines, but a file with
# dozens of tracks could theoretically interleave. We accept that risk for
# the simpler implementation; if corruption is observed, switch to
# per-worker temp files concatenated after xargs.
worker() {
    local tsv_line="$1"
    local path size mtime_raw mtime _ext
    IFS=$'\t' read -r path size mtime_raw _ext <<<"${tsv_line}"
    mtime=${mtime_raw%.*} # truncate fractional seconds (find -printf %T@ is float)
    local raw
    if ! raw=$(ffprobe -v error -show_streams -show_format -of json "${path}" 2>&1); then
        printf '%s\t%s\n' "${path}" "${raw##*$'\n'}" >>"${failures}"
        return 0
    fi
    local tmp_out tmp_err rc=0
    tmp_out=$(mktemp)
    tmp_err=$(mktemp)
    trap 'rm -f "${tmp_out}" "${tmp_err}"' RETURN
    printf '%s' "${raw}" | jq -c \
        --arg path "${path}" \
        --arg size "${size}" \
        --arg mtime "${mtime}" \
        -f "${filter}" >"${tmp_out}" 2>"${tmp_err}" || rc=$?
    if ((rc == 0)); then
        cat "${tmp_out}" >>"${ndjson}"
    else
        # Strip trailing newlines from stderr; replace internal newlines with spaces
        # so the failures row stays one line.
        err=$(tr '\n' ' ' <"${tmp_err}" | sed 's/[[:space:]]*$//')
        printf '%s\tjq: %s\n' "${path}" "${err}" >>"${failures}"
    fi
}
export -f worker
export filter ndjson failures

awk -F'\t' 'NR>1 && $4 != "vbak" {print}' "${manifest}" |
    xargs -d$'\n' -P "${workers}" -I{} bash -c 'worker "$@"' _ {}

ok=$(wc -l <"${ndjson}")
fail=$(wc -l <"${failures}")
echo "probe: ${ok} ok, ${fail} failed"
