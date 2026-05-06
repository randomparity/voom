#!/usr/bin/env bash
# Captures a library snapshot: manifest, extension tally, size totals.
# Usage: snapshot.sh <library-root> <output-dir>
set -euo pipefail

lib_root="${1:?library root required}"
out_dir="${2:?output dir required}"

if [[ ! -d "${lib_root}" ]]; then
    echo "snapshot: library root does not exist: ${lib_root}" >&2
    exit 1
fi
mkdir -p "${out_dir}"

# Extensions VOOM scans plus .vbak (VOOM's keep_backups convention:
# <dir>/.voom-backup/<basename>.<timestamp>.vbak).
exts=(mkv mp4 avi m4v mov ts webm vbak)
find_args=()
for i in "${!exts[@]}"; do
    if ((i > 0)); then find_args+=(-o); fi
    find_args+=(-iname "*.${exts[i]}")
done

manifest="${out_dir}/library-manifest.tsv"
printf 'path\tsize\tmtime\textension\n' >"${manifest}"
# LC_ALL=C: byte-order sort so downstream join has consistent collation.
find "${lib_root}" -type f \( "${find_args[@]}" \) -printf '%p\t%s\t%T@\t%f\n' |
    awk -F'\t' 'BEGIN{OFS="\t"} {
        n = split($4, parts, "."); ext = tolower(parts[n]);
        print $1, $2, $3, ext
      }' |
    LC_ALL=C sort -k1,1 \
        >>"${manifest}"

awk -F'\t' 'NR>1 {c[$4]++} END {for (e in c) print c[e], e}' "${manifest}" |
    sort -rn >"${out_dir}/ext-tally.txt"

awk -F'\t' 'NR>1 {b[$4]+=$2; t+=$2} END {
        for (e in b) printf "%-8s %20d\n", e, b[e];
        printf "%-8s %20d\n", "TOTAL", t
    }' "${manifest}" |
    sort >"${out_dir}/size-totals.txt"

count=$(awk -F'\t' 'NR>1' "${manifest}" | wc -l)
echo "snapshot: ${count} files captured under ${out_dir}"
