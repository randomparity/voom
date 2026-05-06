#!/usr/bin/env bash
# Starts voom serve, hits a fixed list of endpoints, captures statuses + body
# samples to <out-dir>, then shuts the server down.
# Usage: web-smoke.sh <voom-bin> <out-dir>
set -euo pipefail

voom_bin="${1:?voom binary path required}"
out_dir="${2:?output dir required}"

mkdir -p "${out_dir}"
port="${WEB_SMOKE_PORT:-18080}"
log="${out_dir}/serve.log"

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
printf 'endpoint\tstatus\n' >"${statuses}"

probe() {
    local label="$1"
    local url="$2"
    local body_path="${out_dir}/${label}.body"
    local status
    # `|| true` keeps the script alive when curl exits non-zero (e.g. SSE
    # times out on the open stream). The curl output is the HTTP code from
    # the response headers — even when the body never finishes. If curl
    # produced nothing (connection refused before headers), default to 000.
    status=$(curl -s -o "${body_path}" -w '%{http_code}' --max-time 8 "${url}" || true)
    [[ -z "${status}" ]] && status="000"
    printf '%s\t%s\n' "${label}" "${status}" >>"${statuses}"
}

probe root "http://127.0.0.1:${port}/"
probe api-files "http://127.0.0.1:${port}/api/files"
probe api-jobs "http://127.0.0.1:${port}/api/jobs"
probe "events(sse)" "http://127.0.0.1:${port}/events"

cat "${statuses}"
