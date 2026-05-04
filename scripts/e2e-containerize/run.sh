#!/usr/bin/env bash
# Top-level driver for the E2E containerize test.
set -euo pipefail

usage() {
    cat <<EOF
Usage: $0 [--library DIR] [--policy PATH] [--run-dir DIR] [--no-build] [--no-web]

Defaults:
  --library  /mnt/raid0/media/series
  --policy   ~/.config/voom/policies/01-containerize.voom
  --run-dir  ~/voom-e2e-runs/<timestamp>
EOF
}

library="/mnt/raid0/media/series"
policy="${HOME}/.config/voom/policies/01-containerize.voom"
run_dir="${HOME}/voom-e2e-runs/$(date +%Y-%m-%d-%H%M%S)"
do_build=1
do_web=1

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
    --no-build)
        do_build=0
        shift
        ;;
    --no-web)
        do_web=0
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

repo_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
lib_dir="${repo_root}/scripts/e2e-containerize/lib"
voom_bin="${repo_root}/target/release/voom"

mkdir -p "${run_dir}"/{pre,post,logs,reports,db-export,web-smoke}
echo "Run dir: ${run_dir}"

# Helper: run a CLI invocation, capture stdout+stderr to logs/<name>.log
# and the exit code to logs/<name>.log.rc. Does NOT abort on non-zero;
# the summary builder consumes the .rc files.
log_run() {
    local name="$1"
    shift
    local rc=0
    "$@" >"${run_dir}/logs/${name}.log" 2>&1 || rc=$?
    echo "${rc}" >"${run_dir}/logs/${name}.log.rc"
    return 0
}

# ---- Pre-flight ----
echo "==> Pre-flight"
"${lib_dir}/preflight.sh" "${policy}"

if ((do_build)); then
    echo "==> cargo build --release --workspace"
    (cd "${repo_root}" && cargo build --release --workspace) \
        2>&1 | tee "${run_dir}/logs/build.log"
fi

[[ -x "${voom_bin}" ]] || {
    echo "voom binary not found at ${voom_bin}" >&2
    exit 1
}

log_run version "${voom_bin}" --version
log_run doctor "${voom_bin}" doctor
log_run health "${voom_bin}" health
log_run policy-validate "${voom_bin}" policy validate "${policy}"

# ---- Pre-snapshot ----
echo "==> Pre-snapshot"
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/pre" |
    tee "${run_dir}/logs/snapshot-pre.log"
pre_count=$(awk -F'\t' 'NR>1' "${run_dir}/pre/library-manifest.tsv" | wc -l)

# ---- Discovery + introspection ----
echo "==> voom scan"
log_run scan "${voom_bin}" scan "${library}"

log_run files "${voom_bin}" files

# Spot-check inspect on up to 3 sample paths (one of each common ext)
sample_paths=$(awk -F'\t' 'NR>1 && $4=="mkv" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
sample_paths+=$'\n'$(awk -F'\t' 'NR>1 && $4=="mp4" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
sample_paths+=$'\n'$(awk -F'\t' 'NR>1 && $4=="avi" {print $1; exit}' \
    "${run_dir}/pre/library-manifest.tsv")
i=0
while IFS= read -r p; do
    [[ -z "${p}" ]] && continue
    log_run "inspect-${i}" "${voom_bin}" inspect "${p}"
    i=$((i + 1))
done <<<"${sample_paths}"

# ---- Planning + execution ----
echo "==> voom plans (preview)"
log_run plans-preview "${voom_bin}" plans --policy "${policy}"

run_start=$(date -Iseconds)
echo "==> voom process (long run starts at ${run_start})"
log_run process "${voom_bin}" process --policy "${policy}"

log_run jobs "${voom_bin}" jobs

# ---- Post-run inspection ----
echo "==> Post-run inspection"
log_run events "${voom_bin}" events --since "${run_start}"
cp "${run_dir}/logs/events.log" "${run_dir}/reports/events.jsonl"

log_run report "${voom_bin}" report
cp "${run_dir}/logs/report.log" "${run_dir}/reports/report.txt"
log_run history "${voom_bin}" history
cp "${run_dir}/logs/history.log" "${run_dir}/reports/history.txt"
cp "${run_dir}/logs/files.log" "${run_dir}/reports/files.txt"
cp "${run_dir}/logs/plans-preview.log" "${run_dir}/reports/plans.txt"
cp "${run_dir}/logs/jobs.log" "${run_dir}/reports/jobs.txt"

db_path="${HOME}/.config/voom/voom.db"
if [[ -f "${db_path}" ]]; then
    "${lib_dir}/db-export.sh" "${db_path}" "${run_dir}/db-export"
else
    echo "WARN: ${db_path} not found post-run" >&2
fi

# ---- Post-snapshot ----
echo "==> Post-snapshot"
"${lib_dir}/snapshot.sh" "${library}" "${run_dir}/post" |
    tee "${run_dir}/logs/snapshot-post.log"
post_count=$(awk -F'\t' 'NR>1' "${run_dir}/post/library-manifest.tsv" | wc -l)

# ---- Diff ----
echo "==> Diff"
"${lib_dir}/diff-snapshots.sh" "${run_dir}/pre" "${run_dir}/post" \
    "${run_dir}/diff-summary.md"

# ---- Web smoke ----
if ((do_web)); then
    echo "==> Web smoke-test"
    "${lib_dir}/web-smoke.sh" "${voom_bin}" "${run_dir}/web-smoke" \
        2>&1 | tee "${run_dir}/logs/web-smoke.log" ||
        echo "web smoke failed; see logs/web-smoke.log"
fi

# ---- Summary ----
echo "==> Build summary"
"${lib_dir}/build-summary.sh" "${run_dir}" "${pre_count}" "${post_count}"

echo "Done. ${run_dir}/summary.md"
