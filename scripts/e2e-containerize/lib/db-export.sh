#!/usr/bin/env bash
# Exports the post-run VOOM SQLite database to schema + per-table TSV.
# Usage: db-export.sh <db-path> <out-dir>
set -euo pipefail

db="${1:?db path required}"
out="${2:?output dir required}"

if [[ ! -r "${db}" ]]; then
    echo "db-export: not readable: ${db}" >&2
    exit 1
fi
mkdir -p "${out}"

sqlite3 "${db}" '.schema' >"${out}/schema.sql"

mapfile -t tables < <(sqlite3 "${db}" \
    "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;")

for t in "${tables[@]}"; do
    sqlite3 -header -separator $'\t' "${db}" "SELECT * FROM \"${t}\";" \
        >"${out}/${t}.tsv"
done

echo "db-export: wrote schema + ${#tables[@]} tables to ${out}"
