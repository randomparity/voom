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
# row to ffprobe-failures.tsv on parse failure. Writes are append-only and
# small (<8KB typically) so they're atomic on POSIX filesystems.
worker() {
    local tsv_line="$1"
    local path size mtime
    path=$(printf '%s' "${tsv_line}" | awk -F'\t' '{print $1}')
    size=$(printf '%s' "${tsv_line}" | awk -F'\t' '{print $2}')
    mtime=$(printf '%s' "${tsv_line}" | awk -F'\t' '{print int($3)}')
    local raw
    if ! raw=$(ffprobe -v error -show_streams -show_format -of json "${path}" 2>&1); then
        printf '%s\t%s\n' "${path}" "${raw##*$'\n'}" >>"${failures}"
        return 0
    fi
    if ! printf '%s' "${raw}" | jq -c \
        --arg path "${path}" \
        --arg size "${size}" \
        --arg mtime "${mtime}" \
        -f "${filter}" >>"${ndjson}" 2>/dev/null; then
        printf '%s\t%s\n' "${path}" "jq mapping failed" >>"${failures}"
    fi
}
export -f worker
export filter ndjson failures

awk -F'\t' 'NR>1 && $4 != "vbak" {print}' "${manifest}" |
    xargs -P "${workers}" -I{} bash -c 'worker "$@"' _ {}

ok=$(wc -l <"${ndjson}")
fail=$(wc -l <"${failures}")
echo "probe: ${ok} ok, ${fail} failed"
