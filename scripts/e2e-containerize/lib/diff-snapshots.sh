#!/usr/bin/env bash
# Compares pre/ and post/ snapshots and emits diff-summary.md.
# Usage: diff-snapshots.sh <pre-dir> <post-dir> <out-md>
set -euo pipefail

pre="${1:?pre snapshot dir required}"
post="${2:?post snapshot dir required}"
out="${3:?output md path required}"

pre_m="${pre}/library-manifest.tsv"
post_m="${post}/library-manifest.tsv"
[[ -r "${pre_m}" ]] || {
    echo "missing ${pre_m}" >&2
    exit 1
}
[[ -r "${post_m}" ]] || {
    echo "missing ${post_m}" >&2
    exit 1
}

# Path-only sorted lists for set ops
pre_paths=$(mktemp)
post_paths=$(mktemp)
classify_tmp=$(mktemp)
trap 'find "${pre_paths}" "${post_paths}" "${classify_tmp}" -maxdepth 0 -delete 2>/dev/null || true' EXIT

awk -F'\t' 'NR>1 {print $1}' "${pre_m}" | sort >"${pre_paths}"
awk -F'\t' 'NR>1 {print $1}' "${post_m}" | sort >"${post_paths}"

disappeared=$(comm -23 "${pre_paths}" "${post_paths}" | wc -l)
new_files=$(comm -13 "${pre_paths}" "${post_paths}" | wc -l)
common=$(comm -12 "${pre_paths}" "${post_paths}" | wc -l)

# Classify common files: unchanged / mtime-changed / size-changed
join -t $'\t' -j1 \
    <(awk -F'\t' 'NR>1 {print $1"\t"$2"\t"$3}' "${pre_m}" | sort -k1,1) \
    <(awk -F'\t' 'NR>1 {print $1"\t"$2"\t"$3}' "${post_m}" | sort -k1,1) |
    awk -F'\t' 'BEGIN{u=0;m=0;s=0}
        { if ($2==$4 && $3==$5) u++;
          else if ($2!=$4) s++;
          else m++; }
        END {print u"\t"m"\t"s}' \
        >"${classify_tmp}"
read -r unchanged mtime_only size_changed <"${classify_tmp}"

# Per-extension count delta
ext_delta=$(
    join -t $'\t' -a 1 -a 2 -e '0' -o '0,1.2,2.2' \
        <(awk '{print $2"\t"$1}' "${pre}/ext-tally.txt" | sort -k1,1) \
        <(awk '{print $2"\t"$1}' "${post}/ext-tally.txt" | sort -k1,1)
)

# keep_backups invariant: every pre non-MKV must have a counterpart under
# <dir>/.voom-backup/<basename>.<timestamp>.vbak (VOOM's actual convention).
nonmkv_pre="${pre}/non-mkv-files.txt"
missing_bak=0
while IFS= read -r src; do
    [[ -z "${src}" ]] && continue
    src_dir=$(dirname "${src}")
    src_base=$(basename "${src}")
    backup_dir="${src_dir}/.voom-backup"
    if ! compgen -G "${backup_dir}/${src_base}.*.vbak" >/dev/null; then
        missing_bak=$((missing_bak + 1))
    fi
done <"${nonmkv_pre}"

# Bytes delta
pre_bytes=$(awk '/^TOTAL/ {print $2}' "${pre}/size-totals.txt")
post_bytes=$(awk '/^TOTAL/ {print $2}' "${post}/size-totals.txt")
bytes_delta=$((post_bytes - pre_bytes))

nonmkv_count=$(wc -l <"${nonmkv_pre}")

# Render markdown
{
    echo "# Snapshot Diff Summary"
    echo
    echo "## Path-level classification"
    echo
    echo "| Class | Count |"
    echo "|-------|-------|"
    echo "| Unchanged (size + mtime equal) | ${unchanged} |"
    echo "| mtime-changed (size equal) | ${mtime_only} |"
    echo "| size-changed | ${size_changed} |"
    echo "| Disappeared (in pre, not in post) | ${disappeared} |"
    echo "| New (in post, not in pre) | ${new_files} |"
    echo "| Common path total | ${common} |"
    echo
    echo "## Per-extension delta"
    echo
    echo "| Extension | Pre | Post |"
    echo "|-----------|-----|------|"
    echo "${ext_delta}" | awk -F'\t' '{printf "| %s | %s | %s |\n", $1, $2, $3}'
    echo
    echo "## Bytes"
    echo
    echo "| Metric | Bytes |"
    echo "|--------|-------|"
    echo "| Pre total | ${pre_bytes} |"
    echo "| Post total | ${post_bytes} |"
    echo "| Delta | ${bytes_delta} |"
    echo
    echo "## keep_backups invariant"
    echo
    echo "Pre non-MKV files: ${nonmkv_count}"
    echo "Missing backup post-run: ${missing_bak}"
} >"${out}"

echo "diff-snapshots: wrote ${out}"
