#!/usr/bin/env bash
# Exports a VOOM SQLite database: schema.sql plus per-table .tsv dumps.
# Usage: db-export.sh <db-path> <out-dir>
set -euo pipefail

db_path="${1:?db path required}"
out_dir="${2:?output dir required}"

if [[ ! -r "${db_path}" ]]; then
    echo "db-export: ${db_path} not readable" >&2
    exit 1
fi
mkdir -p "${out_dir}"

sqlite3 "${db_path}" '.schema' >"${out_dir}/schema.sql"

# Tables we care about for the audit. plugin_data is excluded — opaque BLOBs
# per plugin, not useful for diffing.
tables=(files tracks jobs plans file_transitions bad_files discovered_files)
for t in "${tables[@]}"; do
    out="${out_dir}/${t}.tsv"
    sqlite3 -header -separator $'\t' "${db_path}" "SELECT * FROM ${t};" >"${out}"
done

echo "db-export: wrote schema.sql + ${#tables[@]} tables to ${out_dir}"
