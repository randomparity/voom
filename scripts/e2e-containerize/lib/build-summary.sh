#!/usr/bin/env bash
# Builds the final summary.md: PASS/WARN/FAIL verdict + anomaly section.
# Reads inputs from the run dir and writes summary.md at its root.
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
    if [[ "${verdict}" == "PASS" ]]; then
        verdict="WARN"
    fi
}

# Hard checks on logged exit codes
for log_name in doctor policy-validate; do
    rc_file="${run}/logs/${log_name}.log.rc"
    if [[ ! -f "${rc_file}" ]]; then
        note_fail "missing ${log_name}.log.rc"
        continue
    fi
    rc=$(cat "${rc_file}")
    if [[ "${rc}" != "0" ]]; then
        note_fail "${log_name} exit code ${rc}"
    fi
done

scan_rc_file="${run}/logs/scan.log.rc"
if [[ ! -f "${scan_rc_file}" ]] || [[ "$(cat "${scan_rc_file}")" != "0" ]]; then
    note_fail "voom scan failed (see logs/scan.log)"
fi

if ((post_count < pre_count)); then
    note_warn "post file count (${post_count}) < pre (${pre_count}) — verify .bak invariant"
fi

# diff-summary signals
diff_md="${run}/diff-summary.md"
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

    if ((missing_bak > 0)); then
        note_fail "${missing_bak} non-MKV source(s) lack a backup under .voom-backup/ (potential data loss)"
    fi
    if ((disappeared > 0)) && ((missing_bak == 0)); then
        note_warn "${disappeared} path(s) disappeared (all accounted for via .voom-backup/)"
    fi
    if ((size_changed > 0)); then
        note_warn "${size_changed} common path(s) changed size — confirm none are pre-existing MKVs"
    fi
else
    note_fail "diff-summary.md not generated"
fi

# Web smoke statuses
statuses="${run}/web-smoke/statuses.tsv"
if [[ -f "${statuses}" ]]; then
    while IFS=$'\t' read -r ep st; do
        [[ "${ep}" == "endpoint" ]] && continue
        # /events is an open SSE stream; we only require headers came back 2xx
        # before max-time closed the connection. Any non-2xx is informational.
        if [[ "${ep}" == *"(sse)"* ]]; then
            if [[ ! "${st}" =~ ^2[0-9][0-9]$ ]]; then
                note_warn "SSE smoke ${ep} returned ${st}"
            fi
            continue
        fi
        if [[ ! "${st}" =~ ^2[0-9][0-9]$ ]]; then
            note_fail "web smoke ${ep} returned ${st}"
        fi
    done <"${statuses}"
else
    note_warn "web-smoke statuses.tsv missing"
fi

# Job stragglers (if jobs report exists)
jobs_report="${run}/reports/jobs.txt"
failed_jobs=0
if [[ -f "${jobs_report}" ]]; then
    if grep -Eqi '\b(running|pending)\b' "${jobs_report}"; then
        note_fail "jobs report contains non-terminal states (running/pending)"
    fi
    failed_jobs=$(grep -Ec '\bfailed\b' "${jobs_report}" || true)
    if ((failed_jobs > 0)); then
        note_warn "${failed_jobs} job(s) reported as failed (see reports/jobs.txt)"
    fi
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
    echo "## Anomaly section"
    echo
    if [[ -f "${jobs_report}" ]]; then
        echo "### Failed jobs"
        echo '```'
        if grep -E '\bfailed\b' "${jobs_report}" | head -50; then :; else echo "(none)"; fi
        echo '```'
    fi
    echo
    echo "### Top 10 longest-running jobs"
    if [[ -f "${run}/db-export/jobs.tsv" ]]; then
        echo '```'
        awk -F'\t' 'NR==1 {for(i=1;i<=NF;i++) h[$i]=i} NR>1 {
            d = ($h["completed_at"] - $h["started_at"]);
            print d "\t" $h["id"] "\t" $h["status"];
        }' "${run}/db-export/jobs.tsv" 2>/dev/null |
            sort -rn | head -10 || echo "(jobs.tsv missing expected columns)"
        echo '```'
    else
        echo "(no jobs.tsv in db-export)"
    fi
    echo
    echo "## Linked artifacts"
    echo
    echo "- [diff-summary.md](diff-summary.md)"
    echo "- [logs/](logs/)"
    echo "- [reports/](reports/)"
    echo "- [db-export/](db-export/)"
    echo "- [web-smoke/](web-smoke/)"
} >"${run}/summary.md"

echo "build-summary: ${verdict} — see ${run}/summary.md"
