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

mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke,diffs}
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
log_run doctor "${voom_bin}" doctor
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
log_run process "${voom_bin}" process -y --on-error continue --policy "${policy}" "${library}"
stage_end process "$t"
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
fi

log_run events "${voom_bin}" events -n 1000000 -f json
cp "${run_dir}/logs/events.log" "${run_dir}/reports/events.json"
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
