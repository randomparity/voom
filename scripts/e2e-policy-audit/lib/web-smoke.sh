#!/usr/bin/env bash
# Starts voom serve, hits a fixed list of endpoints, captures statuses + body
# samples to <out-dir>, then shuts the server down.
# Usage: web-smoke.sh <voom-bin> <out-dir> [db-path]
set -euo pipefail

validate_root_body() {
    local body="$1"
    grep -Eq '<title>VOOM|VOOM' "${body}"
}

validate_files_body() {
    local body="$1"
    jq -e '
        (.files | type == "array" and length >= 1) and
        (.files[0].id | type == "string") and
        (.files[0].path | type == "string") and
        (.total | type == "number")
    ' "${body}" >/dev/null
}

validate_jobs_body() {
    local body="$1"
    local expected_failed="$2"
    jq -e --argjson expected "${expected_failed}" '
        (.jobs | type == "array") and
        (.total == $expected) and
        all(.jobs[]; .status == "failed")
    ' "${body}" >/dev/null
}

validate_sse_body() {
    local body="$1"
    local line
    local payload

    grep -q '^event:' "${body}" || return 1
    grep -q '^data:' "${body}" || return 1

    while IFS= read -r line; do
        [[ "${line}" == data:* ]] || continue
        payload="${line#data:}"
        payload="${payload# }"
        if jq -e . >/dev/null 2>&1 <<<"${payload}"; then
            return 0
        fi
    done <"${body}"

    return 1
}

failed_job_count_from_db() {
    local db_path="$1"

    if [[ -z "${db_path}" || ! -f "${db_path}" ]]; then
        printf '0\n'
        return 0
    fi
    if ! command -v sqlite3 >/dev/null 2>&1; then
        printf '0\n'
        return 0
    fi

    sqlite3 "${db_path}" "select count(*) from jobs where status = 'failed';" 2>/dev/null ||
        printf '0\n'
}

if [[ "${WEB_SMOKE_TEST_MODE:-0}" == "1" ]]; then
    return 0
fi

voom_bin="${1:?voom binary path required}"
out_dir="${2:?output dir required}"
db_path="${3:-}"

mkdir -p "${out_dir}"
port="${WEB_SMOKE_PORT:-18080}"
log="${out_dir}/serve.log"
failed_jobs="$(failed_job_count_from_db "${db_path}")"

"${voom_bin}" serve --port "${port}" >"${log}" 2>&1 &
serve_pid=$!
trap 'kill "${serve_pid}" 2>/dev/null || true; wait "${serve_pid}" 2>/dev/null || true' EXIT

# Wait up to 10s for the server to come up; detect early death.
for _ in {1..20}; do
    if ! kill -0 "${serve_pid}" 2>/dev/null; then
        echo "web-smoke: voom serve died before binding (see ${log})" >&2
        exit 1
    fi
    if curl -fsS "http://127.0.0.1:${port}/" >/dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

statuses="${out_dir}/statuses.tsv"
printf 'endpoint\tstatus\tcontent\n' >"${statuses}"

probe() {
    local label="$1"
    local url="$2"
    local validator="$3"
    shift 3
    local body_path="${out_dir}/${label}.body"
    local status
    local content="FAIL"
    # `|| true` keeps the script alive when curl exits non-zero (e.g. SSE
    # times out on the open stream). The curl output is the HTTP code from
    # the response headers — even when the body never finishes. If curl
    # produced nothing (connection refused before headers), default to 000.
    status=$(curl -s -o "${body_path}" -w '%{http_code}' --max-time 8 "${url}" || true)
    [[ -z "${status}" ]] && status="000"
    if "${validator}" "${body_path}" "$@"; then
        content="PASS"
    fi
    printf '%s\t%s\t%s\n' "${label}" "${status}" "${content}" >>"${statuses}"
}

probe_sse() {
    local label="$1"
    local url="$2"
    local body_path="${out_dir}/${label}.body"
    local status
    local content="FAIL"

    status=$(curl -s -N -o "${body_path}" -w '%{http_code}' --max-time 5 "${url}" || true)
    [[ -z "${status}" ]] && status="000"
    if validate_sse_body "${body_path}"; then
        content="PASS"
    fi
    printf '%s\t%s\t%s\n' "${label}" "${status}" "${content}" >>"${statuses}"
}

probe root "http://127.0.0.1:${port}/" validate_root_body
probe api-files "http://127.0.0.1:${port}/api/files?limit=1" validate_files_body
probe api-jobs "http://127.0.0.1:${port}/api/jobs?status=failed" validate_jobs_body "${failed_jobs}"
probe_sse "events(sse)" "http://127.0.0.1:${port}/events"

cat "${statuses}"

if awk -F'\t' 'NR > 1 && ($2 !~ /^2[0-9][0-9]$/ || $3 != "PASS") { bad = 1 } END { exit bad }' "${statuses}"; then
    exit 0
fi
exit 1
