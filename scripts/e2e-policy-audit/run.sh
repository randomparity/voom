#!/usr/bin/env bash
# Top-level driver for the E2E policy audit harness.
set -euo pipefail

usage() {
    cat <<EOF
Usage: $0 [--library DIR] [--policy PATH] [--run-dir DIR]
          [--probe-workers N] [--no-build] [--no-web] [--no-probe]

Defaults:
  --library         /mnt/raid0/media/series
  --policy          ~/.config/voom/policies/02-hw-transcode-hevc.voom
  --run-dir         ~/voom-e2e-runs/<timestamp>-<policy-stem>
  --probe-workers   8
EOF
}

library="/mnt/raid0/media/series"
policy="${HOME}/.config/voom/policies/02-hw-transcode-hevc.voom"
run_dir=""
probe_workers=8
do_build=1
do_web=1
do_probe=1

while (($# > 0)); do
    case "$1" in
    --library)
        library="$2"
        shift 2
        ;;
    --policy)
        policy="$2"
        shift 2
        ;;
    --run-dir)
        run_dir="$2"
        shift 2
        ;;
    --probe-workers)
        probe_workers="$2"
        shift 2
        ;;
    --no-build)
        do_build=0
        shift
        ;;
    --no-web)
        do_web=0
        shift
        ;;
    --no-probe)
        do_probe=0
        shift
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "unknown arg: $1" >&2
        usage
        exit 2
        ;;
    esac
done

policy_stem=$(basename "${policy}" .voom)
if [[ -z "${run_dir}" ]]; then
    run_dir="${HOME}/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)-${policy_stem}"
fi

repo_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
lib_dir="${repo_root}/scripts/e2e-policy-audit/lib"
voom_bin="${repo_root}/target/release/voom"

mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke,diffs,runtime} "${run_dir}/logs/env-check" "${run_dir}/logs/plugin-errors"
echo "Run dir: ${run_dir}"

stage_start() { date +%s.%N; }
stage_end() {
    local name="$1" start="$2"
    local now elapsed
    now=$(date +%s.%N)
    elapsed=$(awk -v s="${start}" -v n="${now}" 'BEGIN{printf "%.2f", n - s}')
    python3 - "${run_dir}/manifest.json" "${name}" "${elapsed}" <<'PY'
import json, sys
path, name, elapsed = sys.argv[1], sys.argv[2], float(sys.argv[3])
with open(path) as f: m = json.load(f)
m["stages"][name] = elapsed
with open(path, "w") as f: json.dump(m, f, indent=2)
PY
}

log_run() {
    local name="$1"
    shift
    local rc=0
    "$@" >"${run_dir}/logs/${name}.log" 2>&1 || rc=$?
    echo "${rc}" >"${run_dir}/logs/${name}.log.rc"
    return 0
}

runtime_sampler_pid=""
env_check_sampler_pid=""
runtime_sampler_uses_group=0
env_check_sampler_uses_group=0
active_process_pid=""
active_process_uses_group=0

start_background_session() {
    if command -v setsid >/dev/null 2>&1; then
        setsid "$@" &
        return 0
    fi
    "$@" &
    return 1
}

process_is_live() {
    local pid="$1" uses_group="$2"
    if ((uses_group)); then
        kill -0 "-${pid}" 2>/dev/null
    else
        kill -0 "${pid}" 2>/dev/null
    fi
}

signal_process() {
    local pid="$1" uses_group="$2" signal="$3"
    if [[ -z "${pid}" ]]; then
        return 0
    fi
    if ((uses_group)); then
        kill -"${signal}" "-${pid}" 2>/dev/null || true
    else
        kill -"${signal}" "${pid}" 2>/dev/null || true
    fi
}

stop_process_tree() {
    local pid="$1" uses_group="$2"
    local initial_signal="${3:-TERM}"
    local i

    if [[ -z "${pid}" ]]; then
        return 0
    fi

    signal_process "${pid}" "${uses_group}" "${initial_signal}"
    for _ in {1..5}; do
        if ! process_is_live "${pid}" "${uses_group}"; then
            wait "${pid}" 2>/dev/null || true
            return 0
        fi
        sleep 1
    done

    signal_process "${pid}" "${uses_group}" KILL
    for i in {1..5}; do
        if ! process_is_live "${pid}" "${uses_group}"; then
            break
        fi
        sleep 1
    done
    wait "${pid}" 2>/dev/null || true
}

stop_process_samplers() {
    stop_process_tree "${runtime_sampler_pid}" "${runtime_sampler_uses_group}"
    runtime_sampler_pid=""
    runtime_sampler_uses_group=0

    stop_process_tree "${env_check_sampler_pid}" "${env_check_sampler_uses_group}"
    env_check_sampler_pid=""
    env_check_sampler_uses_group=0
}

start_process_samplers() {
    if start_background_session "${lib_dir}/runtime-sampler.sh" "${run_dir}" 300 "${voom_bin}"; then
        runtime_sampler_uses_group=1
    else
        runtime_sampler_uses_group=0
    fi
    runtime_sampler_pid=$!
    if start_background_session "${lib_dir}/env-check-sampler.sh" "${run_dir}" "${voom_bin}" 3600; then
        env_check_sampler_uses_group=1
    else
        env_check_sampler_uses_group=0
    fi
    env_check_sampler_pid=$!
}

forward_process_signal() {
    local signal="$1" exit_code="$2"
    trap - EXIT INT TERM
    stop_process_tree "${active_process_pid}" "${active_process_uses_group}" "${signal}"
    active_process_pid=""
    active_process_uses_group=0
    stop_process_samplers
    if [[ ! -f "${run_dir}/logs/process.log.rc" ]]; then
        echo "${exit_code}" >"${run_dir}/logs/process.log.rc"
    fi
    exit "${exit_code}"
}

run_process_command() {
    local rc=0

    active_process_pid=""
    active_process_uses_group=0
    rm -f "${run_dir}/logs/process.log.rc"

    if start_background_session "$@"; then
        active_process_uses_group=1
    else
        active_process_uses_group=0
    fi >"${run_dir}/logs/process.log" 2>&1
    active_process_pid=$!

    trap 'stop_process_samplers' EXIT
    trap 'forward_process_signal INT 130' INT
    trap 'forward_process_signal TERM 143' TERM

    wait "${active_process_pid}" || rc=$?
    echo "${rc}" >"${run_dir}/logs/process.log.rc"
    active_process_pid=""
    active_process_uses_group=0
    return 0
}

# ---- Preflight ----
echo "==> Preflight"
t=$(stage_start)
"${lib_dir}/preflight.sh" "${policy}" "${library}" "${run_dir}" "${voom_bin}"
stage_end preflight "$t"

if ((do_build)); then
    echo "==> cargo build --release --workspace"
    t=$(stage_start)
    (cd "${repo_root}" && cargo build --release --workspace) \
        2>&1 | tee "${run_dir}/logs/build.log"
    stage_end build "$t"
fi

[[ -x "${voom_bin}" ]] || {
    echo "voom binary not found at ${voom_bin}" >&2
    exit 1
}

log_run version "${voom_bin}" --version
log_run env-check "${voom_bin}" env check
log_run policy-validate "${voom_bin}" policy validate "${policy}"

# ---- Pre snapshot + probe ----
echo "==> Pre-snapshot"
t=$(stage_start)
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/pre"
stage_end snapshot_pre "$t"
pre_count=$(awk -F'\t' 'NR>1' "${run_dir}/pre/library-manifest.tsv" | wc -l)

if ((do_probe)); then
    echo "==> Pre-probe (${probe_workers} workers)"
    t=$(stage_start)
    "${lib_dir}/probe.sh" "${library}" "${run_dir}/pre" "${probe_workers}"
    stage_end probe_pre "$t"
fi

# ---- Discover + introspect ----
echo "==> voom scan"
t=$(stage_start)
log_run scan "${voom_bin}" scan -r -y "${library}"
stage_end scan "$t"

log_run files-list "${voom_bin}" files list -f csv
cp "${run_dir}/logs/files-list.log" "${run_dir}/pre/voom-files.csv"

db_path="${HOME}/.config/voom/voom.db"
if [[ -f "${db_path}" ]]; then
    "${lib_dir}/db-export.sh" "${db_path}" "${run_dir}/pre/voom-db-tables"
    "${lib_dir}/db-to-ndjson.sh" "${run_dir}/pre/voom-db-tables" "${run_dir}/pre/voom-db.ndjson"
fi

# ---- Plan + execute ----
echo "==> voom process --plan-only"
log_run plans-preview "${voom_bin}" process --plan-only -y --policy "${policy}" "${library}"
cp "${run_dir}/logs/plans-preview.log" "${run_dir}/reports/plans.json"

# shellcheck disable=SC2034 # informational only
run_start=$(date -Iseconds)
echo "==> voom process (long run starts at ${run_start})"
t=$(stage_start)
start_process_samplers
run_process_command "${voom_bin}" process -y --on-error continue --policy "${policy}" "${library}"
stop_process_samplers
trap - EXIT INT TERM
stage_end process "$t"
"${lib_dir}/capture-host-journal.sh" "${run_dir}" "${run_start}" ||
    echo "host journal capture failed (continuing)" >&2
log_run jobs-list "${voom_bin}" jobs list
cp "${run_dir}/logs/jobs-list.log" "${run_dir}/reports/jobs.txt"

# ---- Post snapshot + probe ----
echo "==> Post-snapshot"
t=$(stage_start)
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/post"
stage_end snapshot_post "$t"
post_count=$(awk -F'\t' 'NR>1' "${run_dir}/post/library-manifest.tsv" | wc -l)

if ((do_probe)); then
    echo "==> Post-probe"
    t=$(stage_start)
    "${lib_dir}/probe.sh" "${library}" "${run_dir}/post" "${probe_workers}"
    stage_end probe_post "$t"
fi

if [[ -f "${db_path}" ]]; then
    "${lib_dir}/db-export.sh" "${db_path}" "${run_dir}/post/voom-db-tables"
    "${lib_dir}/db-to-ndjson.sh" "${run_dir}/post/voom-db-tables" "${run_dir}/post/voom-db.ndjson"
    # The build-summary's longest-jobs query reads db-export/jobs.tsv.
    cp -r "${run_dir}/post/voom-db-tables/." "${run_dir}/db-export/"
    if [[ -f "${run_dir}/db-export/plans.tsv" ]]; then
        "${lib_dir}/ffmpeg-stderr-normalize.py" \
            "${run_dir}/db-export/plans.tsv" \
            "${run_dir}/db-export/plans.normalized.tsv" &&
            mv "${run_dir}/db-export/plans.normalized.tsv" "${run_dir}/db-export/plans.tsv" ||
            echo "ffmpeg stderr normalization failed (continuing)" >&2
    fi
fi

log_run events "${voom_bin}" events -n 1000000 -f json
cp "${run_dir}/logs/events.log" "${run_dir}/reports/events.json"
"${lib_dir}/plugin-error-dedupe.py" \
    "${run_dir}/reports/events.json" \
    "${run_dir}/reports/events-deduped.json" \
    "${run_dir}/logs/plugin-errors" \
    "${run_dir}/diffs/plugin-error-summary.md" ||
    echo "plugin error dedupe failed (continuing)" >&2
log_run report "${voom_bin}" report --all
cp "${run_dir}/logs/report.log" "${run_dir}/reports/report.txt"

# ---- Web smoke ----
if ((do_web)); then
    echo "==> Web smoke"
    "${lib_dir}/web-smoke.sh" "${voom_bin}" "${run_dir}/web-smoke" \
        2>&1 | tee "${run_dir}/logs/web-smoke.log" ||
        echo "web smoke failed; see logs/web-smoke.log"
fi

# ---- Diffs ----
echo "==> Diffs"
t=$(stage_start)
"${lib_dir}/diff-snapshots.sh" "${run_dir}/pre" "${run_dir}/post" "${run_dir}/diffs/files-summary.md"
ignore="${lib_dir}/ndjson-ignore.txt"
if ((do_probe)); then
    diff_pids=()
    for combo in \
        "pre/voom-db.ndjson:pre/ffprobe.ndjson:db-vs-ffprobe-pre.tsv" \
        "post/voom-db.ndjson:post/ffprobe.ndjson:db-vs-ffprobe-post.tsv" \
        "pre/voom-db.ndjson:post/voom-db.ndjson:voom-db-pre-vs-post.tsv" \
        "pre/ffprobe.ndjson:post/ffprobe.ndjson:ffprobe-pre-vs-post.tsv"; do
        IFS=: read -r left right out <<<"${combo}"
        if [[ -r "${run_dir}/${left}" && -r "${run_dir}/${right}" ]]; then
            "${lib_dir}/diff-ndjson.py" \
                "${run_dir}/${left}" "${run_dir}/${right}" \
                "${run_dir}/diffs/${out}" --ignore-file "${ignore}" &
            diff_pids+=($!)
        fi
    done
    if [[ -r "${run_dir}/pre/ffprobe.ndjson" && -r "${run_dir}/post/ffprobe.ndjson" ]]; then
        "${lib_dir}/codec-pivot.py" \
            "${run_dir}/pre/ffprobe.ndjson" "${run_dir}/post/ffprobe.ndjson" \
            "${run_dir}/diffs/codec-pivot.md" &
        diff_pids+=($!)
        "${lib_dir}/tracks-pivot.py" \
            "${run_dir}/pre/ffprobe.ndjson" "${run_dir}/post/ffprobe.ndjson" \
            "${run_dir}/diffs/tracks-pivot.md" &
        diff_pids+=($!)
    fi
    # Wait for all diff/pivot scripts; report failures but continue.
    for pid in "${diff_pids[@]}"; do
        if ! wait "${pid}"; then
            echo "diff/pivot pid ${pid} failed (continuing)" >&2
        fi
    done

    for diff_name in db-vs-ffprobe-post ffprobe-pre-vs-post; do
        diff_tsv="${run_dir}/diffs/${diff_name}.tsv"
        if [[ -r "${diff_tsv}" ]]; then
            "${lib_dir}/diff-class-summary.py" \
                "${diff_tsv}" \
                "${run_dir}/diffs/${diff_name}-summary.tsv" \
                "${run_dir}/diffs/${diff_name}-summary.md" ||
                echo "diff class summary failed for ${diff_name} (continuing)" >&2
        fi
    done
fi
if compgen -G "${run_dir}/runtime/*.txt" >/dev/null; then
    "${lib_dir}/runtime-timeline.py" \
        "${run_dir}/runtime" \
        "${run_dir}/diffs/runtime-timeline.md" ||
        echo "runtime timeline generation failed (continuing)" >&2
fi
if compgen -G "${run_dir}/logs/env-check/[0-9][0-9][0-9][0-9].log" >/dev/null; then
    "${lib_dir}/env-check-timeline.py" \
        "${run_dir}/logs/env-check" \
        "${run_dir}/diffs/env-check-timeline.md" ||
        echo "env check timeline generation failed (continuing)" >&2
fi
if [[ -f "${run_dir}/db-export/plans.tsv" ]]; then
    "${lib_dir}/failure-timeline.py" \
        "${run_dir}/db-export/plans.tsv" \
        "${run_dir}/diffs/failure-timeline.md" ||
        echo "failure timeline generation failed (continuing)" >&2
fi
stage_end diff "$t"

# Mark completion timestamp
python3 - "${run_dir}/manifest.json" <<'PY'
import json, sys, datetime
path = sys.argv[1]
with open(path) as f: m = json.load(f)
m["completed_at"] = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
with open(path, "w") as f: json.dump(m, f, indent=2)
PY

# ---- Summary ----
echo "==> Build summary"
"${lib_dir}/build-summary.sh" "${run_dir}" "${pre_count}" "${post_count}"

echo "Done. ${run_dir}/summary.md"
