#!/usr/bin/env bash
# Brief smoke-test of `voom serve`: start, curl a fixed endpoint set,
# capture statuses + body samples, shut down.
# Usage: web-smoke.sh <voom-bin> <out-dir>
set -euo pipefail

voom_bin="${1:?voom binary path required}"
out="${2:?output dir required}"
port="${WEB_SMOKE_PORT:-18080}"
mkdir -p "${out}"

log="${out}/serve.log"
"${voom_bin}" serve --port "${port}" >"${log}" 2>&1 &
serve_pid=$!
trap 'kill "${serve_pid}" 2>/dev/null || true; wait "${serve_pid}" 2>/dev/null || true' EXIT

# Wait up to 30s for the server to bind
for _ in $(seq 1 30); do
    if curl -fsS -o /dev/null "http://127.0.0.1:${port}/" 2>/dev/null; then
        break
    fi
    if ! kill -0 "${serve_pid}" 2>/dev/null; then
        echo "web-smoke: server died before binding (see ${log})" >&2
        exit 1
    fi
    sleep 1
done

endpoints=(/ /api/files /api/jobs /api/jobs/stats /api/stats /api/health /api/plugins)
status_file="${out}/statuses.tsv"
printf 'endpoint\tstatus\n' >"${status_file}"
for ep in "${endpoints[@]}"; do
    body_file="${out}/body$(echo "${ep}" | tr '/' '_').txt"
    status=$(curl -s -o "${body_file}" -w '%{http_code}' \
        "http://127.0.0.1:${port}${ep}" || echo "000")
    printf '%s\t%s\n' "${ep}" "${status}" >>"${status_file}"
    # Truncate body samples to 4 KiB
    if [[ -s "${body_file}" ]]; then
        head -c 4096 "${body_file}" >"${body_file}.head"
        mv "${body_file}.head" "${body_file}"
    fi
done

# SSE: open the stream briefly, capture the first chunk, then close.
sse_status=$(curl -s -o "${out}/body_events.txt" -w '%{http_code}' \
    --max-time 3 "http://127.0.0.1:${port}/events" || true)
printf '/events (sse)\t%s\n' "${sse_status:-timeout}" >>"${status_file}"

cat "${status_file}"
echo "web-smoke: artifacts in ${out}"
