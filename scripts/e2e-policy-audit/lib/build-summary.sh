#!/usr/bin/env bash
# Builds summary.md from a populated run dir.
# Usage: build-summary.sh <run-dir> <pre-count> <post-count>
set -euo pipefail

run="${1:?run dir required}"
pre_count="${2:?pre file count required}"
post_count="${3:?post file count required}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

verdict="PASS"
hard_fails=()
soft_warns=()

note_fail() {
  hard_fails+=("$1")
  verdict="FAIL"
}
note_warn() {
  soft_warns+=("$1")
  [[ "${verdict}" == "PASS" ]] && verdict="WARN"
}

# Hard checks on logged exit codes
for log_name in doctor policy-validate scan; do
  rc_file="${run}/logs/${log_name}.log.rc"
  if [[ ! -f "${rc_file}" ]]; then
    note_fail "missing ${log_name}.log.rc"
    continue
  fi
  rc=$(cat "${rc_file}")
  [[ "${rc}" != "0" ]] && note_fail "${log_name} exit code ${rc}"
done

# diff-snapshots data-loss check
diff_md="${run}/diffs/files-summary.md"
missing_bak=0
disappeared=0
size_changed=0
if [[ -f "${diff_md}" ]]; then
  missing_bak=$(awk '/Missing backup post-run:/ {print $NF}' "${diff_md}")
  [[ -z "${missing_bak}" ]] && missing_bak=0
  disappeared=$(awk -F'\\| ' '/Disappeared/ {print $3}' "${diff_md}" | tr -dc '0-9')
  [[ -z "${disappeared}" ]] && disappeared=0
  size_changed=$(awk -F'\\| ' '/size-changed/ {print $3}' "${diff_md}" | tr -dc '0-9')
  [[ -z "${size_changed}" ]] && size_changed=0
  ((missing_bak > 0)) && note_fail "${missing_bak} disappeared source(s) lack a backup under .voom-backup/"
  ((size_changed > 0)) && note_warn "${size_changed} common path(s) changed size"
else
  note_fail "files-summary.md not generated"
fi

((post_count < pre_count)) && note_warn "post file count (${post_count}) < pre (${pre_count})"

# Web smoke
statuses="${run}/web-smoke/statuses.tsv"
if [[ -f "${statuses}" ]]; then
  while IFS=$'\t' read -r ep st; do
    [[ "${ep}" == "endpoint" ]] && continue
    if [[ "${ep}" == *"(sse)"* ]]; then
      [[ ! "${st}" =~ ^2[0-9][0-9]$ ]] && note_warn "SSE smoke ${ep} returned ${st}"
      continue
    fi
    [[ ! "${st}" =~ ^2[0-9][0-9]$ ]] && note_fail "web smoke ${ep} returned ${st}"
  done <"${statuses}"
fi

# Job stragglers
jobs_report="${run}/reports/jobs.txt"
if [[ -f "${jobs_report}" ]]; then
  if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
    note_fail "jobs report contains non-terminal states (running/pending)"
  fi
fi

# Per-phase plan summary from db-export/plans.tsv.
phase_summary="${run}/diffs/phase-summary.tsv"
failed_plans="${run}/diffs/failed-plans.tsv"
failure_clusters="${run}/diffs/failure-clusters.tsv"
failure_clusters_md="${run}/diffs/failure-clusters.md"
plans_tsv="${run}/db-export/plans.tsv"
if [[ -f "${plans_tsv}" ]]; then
  "${script_dir}/plan-phase-summary.py" "${plans_tsv}" "${phase_summary}" "${failed_plans}"
  if [[ -s "${failed_plans}" ]] && [[ $(awk 'END {print NR}' "${failed_plans}") -gt 1 ]]; then
    "${script_dir}/failure-clusters.py" \
      "${failed_plans}" "${failure_clusters}" "${failure_clusters_md}" \
      --files-tsv "${run}/db-export/files.tsv" \
      --pre-ffprobe "${run}/pre/ffprobe.ndjson"
  fi

  while IFS=$'\t' read -r phase total _completed _skipped failed; do
    [[ "${phase}" == "phase" ]] && continue
    if ((failed > 0)); then
      note_fail "phase ${phase}: ${failed}/${total} plans failed"
    fi
  done <"${phase_summary}"
fi

# Render
{
  echo "# E2E Run Summary — ${verdict}"
  echo
  echo "Run dir: \`${run}\`"
  date -Iseconds | sed 's/^/Generated: /'
  echo
  echo "## Counts"
  echo
  echo "- Pre-run files: ${pre_count}"
  echo "- Post-run files: ${post_count}"
  echo
  echo "## Hard criteria"
  echo
  if ((${#hard_fails[@]} == 0)); then
    echo "All passed."
  else
    for f in "${hard_fails[@]}"; do echo "- FAIL: ${f}"; done
  fi
  echo
  echo "## Soft criteria"
  echo
  if ((${#soft_warns[@]} == 0)); then
    echo "No warnings."
  else
    for w in "${soft_warns[@]}"; do echo "- WARN: ${w}"; done
  fi
  echo
  echo "## Per-phase plan summary"
  if [[ -s "${phase_summary}" ]]; then
    echo
    echo '```'
    cat "${phase_summary}"
    echo '```'
  else
    echo
    echo "(plans.tsv not available or not parseable per-phase)"
  fi
  echo
  echo "## Anomaly section"
  echo
  echo "### Failure clusters"
  if [[ -s "${failure_clusters}" ]]; then
    echo '```'
    head -21 "${failure_clusters}"
    echo '```'
  else
    echo "(none)"
  fi
  echo
  echo "### Failed plans (first 20)"
  echo '```'
  if [[ -f "${failed_plans}" ]] && [[ $(awk 'END {print NR}' "${failed_plans}") -gt 1 ]]; then
    head -21 "${failed_plans}"
  else
    echo "(none)"
  fi
  echo '```'
  echo
  echo "### Top 10 longest jobs"
  if [[ -f "${run}/db-export/jobs.tsv" ]]; then
    echo '```'
    awk -F'\t' 'NR==1 {for(i=1;i<=NF;i++) h[$i]=i} NR>1 {
            d = ($h["completed_at"] - $h["started_at"]);
            print d "\t" $h["id"] "\t" $h["status"];
        }' "${run}/db-export/jobs.tsv" 2>/dev/null |
      sort -rn | head -10 || echo "(jobs.tsv missing expected columns)"
    echo '```'
  fi
  echo
  echo "### First 50 db-vs-ffprobe-post divergences (introspection bugs)"
  if [[ -f "${run}/diffs/db-vs-ffprobe-post.tsv" ]]; then
    echo '```'
    head -51 "${run}/diffs/db-vs-ffprobe-post.tsv"
    echo '```'
  fi
  echo
  echo "## Linked artifacts"
  echo
  echo "- [diffs/](diffs/)"
  echo "- [logs/](logs/)"
  echo "- [reports/](reports/)"
  echo "- [db-export/](db-export/)"
  echo "- [web-smoke/](web-smoke/)"
} >"${run}/summary.md"

echo "build-summary: ${verdict} — see ${run}/summary.md"
