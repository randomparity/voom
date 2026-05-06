#!/usr/bin/env bash
# Builds summary.md from a populated run dir.
# Usage: build-summary.sh <run-dir> <pre-count> <post-count>
set -euo pipefail

run="${1:?run dir required}"
pre_count="${2:?pre file count required}"
post_count="${3:?post file count required}"

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

# Job stragglers + per-phase failure rate
jobs_report="${run}/reports/jobs.txt"
failed_jobs=0
if [[ -f "${jobs_report}" ]]; then
    if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
        note_fail "jobs report contains non-terminal states (running/pending)"
    fi
    failed_jobs=$(grep -Ec '\bfailed\b' "${jobs_report}" || true)
    ((failed_jobs > 0)) && note_warn "${failed_jobs} job(s) reported as failed"
fi

# Per-phase 100%-failure check from db-export/jobs.tsv (header-driven column
# lookup so the script doesn't break if column order changes). Phase is
# extracted from the JSON payload column via jq.
phase_summary="${run}/diffs/phase-summary.tsv"
jobs_tsv="${run}/db-export/jobs.tsv"
if [[ -f "${jobs_tsv}" ]]; then
    awk -F'\t' '
        NR==1 { for (i=1; i<=NF; i++) h[$i]=i; next }
        {
            payload = $h["payload"]; status = $h["status"]
            print payload "\t" status
        }
    ' "${jobs_tsv}" |
        while IFS=$'\t' read -r payload status; do
            phase=$(printf '%s' "${payload}" | jq -r '.phase_name // .phase // "unknown"' 2>/dev/null || echo "unknown")
            printf '%s\t%s\n' "${phase}" "${status}"
        done |
        awk -F'\t' '
            { tot[$1]++; if ($2=="failed") fails[$1]++ }
            END {
                print "phase\ttotal\tfailed"
                for (p in tot) printf "%s\t%d\t%d\n", p, tot[p], fails[p]+0
            }
        ' >"${phase_summary}"

    while IFS=$'\t' read -r p tot fail; do
        [[ "${p}" == "phase" ]] && continue
        if ((tot > 0 && fail == tot)); then
            note_fail "phase ${p}: 100% (${fail}/${tot}) jobs failed"
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
    echo "## Per-phase job summary"
    if [[ -s "${phase_summary}" ]]; then
        echo
        echo '```'
        cat "${phase_summary}"
        echo '```'
    else
        echo
        echo "(jobs report not parseable per-phase)"
    fi
    echo
    echo "## Anomaly section"
    echo
    if [[ -f "${jobs_report}" ]]; then
        echo "### Failed jobs (first 50)"
        echo '```'
        grep -E '\bfailed\b' "${jobs_report}" | head -50 || echo "(none)"
        echo '```'
    fi
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
