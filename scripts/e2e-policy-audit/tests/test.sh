#!/usr/bin/env bash
# Runs all diff/pivot scripts against every fixture under tests/fixtures/
# and asserts output matches tests/expected/<scenario>/<artifact>.
set -euo pipefail

cd "$(dirname "$0")/.."
fixtures=(transcode)
fail=0

assert_match() {
  local actual="$1"
  local expected="$2"
  if ! diff -u "${expected}" "${actual}"; then
    echo "FAIL: ${actual} differs from ${expected}" >&2
    fail=1
  fi
}

run_summary_failed_phase_test() {
  local actual
  local normalized
  local expected

  expected="tests/expected/summary-failed-phase"
  actual=$(mktemp -d)
  normalized=$(mktemp -d)
  trap 'rm -R "${actual}" "${normalized}"' EXIT

  mkdir -p \
    "${actual}/logs" \
    "${actual}/reports" \
    "${actual}/db-export" \
    "${actual}/diffs"

  for log_name in doctor policy-validate scan; do
    printf '0\n' >"${actual}/logs/${log_name}.log.rc"
  done

  cat >"${actual}/diffs/files-summary.md" <<'EOF'
# Snapshot Diff Summary

Disappeared paths: 0
Missing backup post-run: 0
EOF

  cat >"${actual}/reports/jobs.txt" <<'EOF'
job-1 completed
job-2 completed
EOF

  cat >"${actual}/db-export/jobs.tsv" <<'EOF'
id	status	payload	started_at	completed_at
job-1	completed	{"job":"process"}	10	15
job-2	completed	{"job":"process"}	20	30
EOF

  cat >"${actual}/db-export/plans.tsv" <<'EOF'
id	file_id	phase_name	status	result
plan-1	file-1	containerize	completed	{"ok":true}
plan-2	file-2	transcode-video	completed	{"ok":true}
plan-3	file-3	transcode-video	skipped	{"reason":"already-compatible"}
plan-4	file-4	transcode-video	failed	{"error":"encoder failed"}
EOF

  "lib/build-summary.sh" "${actual}" 4 4

  assert_match "${actual}/diffs/phase-summary.tsv" "${expected}/phase-summary.tsv"

  sed \
    -e "s|Run dir: \`${actual}\`|Run dir: \`<RUN_DIR>\`|" \
    -e 's/^Generated: .*/Generated: <GENERATED_AT>/' \
    "${actual}/summary.md" >"${normalized}/summary.md"
  assert_match "${normalized}/summary.md" "${expected}/summary.md"

  rm -R "${actual}" "${normalized}"
  trap - EXIT
}

run_summary_failed_phase_test

raw_actual=$(mktemp -d)
trap 'rm -rf "${raw_actual}"' EXIT

sqlite3 "${raw_actual}/voom.db" <<'SQL'
CREATE TABLE files (
    id TEXT PRIMARY KEY,
    path TEXT UNIQUE,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    expected_hash TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    missing_since TEXT,
    superseded_by TEXT,
    container TEXT NOT NULL,
    duration REAL,
    bitrate INTEGER,
    tags TEXT,
    plugin_metadata TEXT,
    introspected_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    stream_index INTEGER NOT NULL,
    track_type TEXT NOT NULL,
    codec TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'und',
    title TEXT NOT NULL DEFAULT '',
    is_default INTEGER NOT NULL DEFAULT 0,
    is_forced INTEGER NOT NULL DEFAULT 0,
    channels INTEGER,
    channel_layout TEXT,
    sample_rate INTEGER,
    bit_depth INTEGER,
    width INTEGER,
    height INTEGER,
    frame_rate REAL,
    is_vfr INTEGER NOT NULL DEFAULT 0,
    is_hdr INTEGER NOT NULL DEFAULT 0,
    hdr_format TEXT,
    pixel_format TEXT
);
CREATE TABLE jobs (id TEXT);
CREATE TABLE plans (id TEXT);
CREATE TABLE file_transitions (id TEXT);
CREATE TABLE bad_files (id TEXT);
CREATE TABLE discovered_files (id TEXT);
INSERT INTO files (
    id, path, filename, size, content_hash, container, duration, bitrate,
    tags, plugin_metadata, introspected_at, created_at, updated_at
) VALUES (
    'file-1', '/lib/show1/S01E03.mkv', 'S01E03.mkv', 800000, 'ghi789',
    'mkv', 1800.0, 3000000, '{}', '{}',
    '2026-05-05T10:00:00Z',
    '2026-05-05T10:00:00Z',
    '2026-05-05T10:00:00Z'
);
INSERT INTO tracks (
    id, file_id, stream_index, track_type, codec, language, title,
    is_default, is_forced, width, height, frame_rate, is_vfr, is_hdr,
    pixel_format
) VALUES (
    'track-1', 'file-1', 0, 'video', 'h264', 'und', '',
    1, 0, 1920, 1080, 30000.0 / 1001.0, 0, 0, 'yuv420p'
);
SQL
"lib/db-export.sh" "${raw_actual}/voom.db" "${raw_actual}/db-export"
awk -F '\t' 'NR == 2 {print $16}' \
  "${raw_actual}/db-export/tracks.tsv" >"${raw_actual}/exported-frame-rate.txt"
printf '29.970029970029969\n' >"${raw_actual}/expected-frame-rate.txt"
assert_match \
  "${raw_actual}/exported-frame-rate.txt" \
  "${raw_actual}/expected-frame-rate.txt"

"lib/db-to-ndjson.sh" "tests/raw-db" "${raw_actual}/raw-db.ndjson"
assert_match "${raw_actual}/raw-db.ndjson" "tests/expected/raw-db.ndjson"

jq -c \
  --arg path "/lib/show1/S01E03.mkv" \
  --arg size "800000" \
  --arg mtime "1234567890" \
  -f "lib/ffprobe-to-ndjson.jq" \
  "tests/raw-ffprobe/issue-258.json" >"${raw_actual}/raw-ffprobe-issue-258.ndjson"
assert_match \
  "${raw_actual}/raw-ffprobe-issue-258.ndjson" \
  "tests/expected/raw-ffprobe-issue-258.ndjson"

awk '$0 ~ /^{"path":"\/lib\/show1\/S01E03\.mkv"/ {print}' \
  "${raw_actual}/raw-db.ndjson" >"${raw_actual}/raw-db-issue-258.ndjson"
"lib/diff-ndjson.py" \
  "${raw_actual}/raw-db-issue-258.ndjson" \
  "${raw_actual}/raw-ffprobe-issue-258.ndjson" \
  "${raw_actual}/issue-258-db-vs-ffprobe.tsv" \
  --ignore-file "lib/ndjson-ignore.txt"
assert_match \
  "${raw_actual}/issue-258-db-vs-ffprobe.tsv" \
  "tests/expected/issue-258-db-vs-ffprobe.tsv"

rm -rf "${raw_actual}"
trap - EXIT

for scenario in "${fixtures[@]}"; do
  pre="tests/fixtures/${scenario}/pre"
  post="tests/fixtures/${scenario}/post"
  expected="tests/expected/${scenario}"
  actual=$(mktemp -d)
  trap 'rm -rf "${actual}"' EXIT

  "lib/diff-snapshots.sh" "${pre}" "${post}" "${actual}/files-summary.md"
  assert_match "${actual}/files-summary.md" "${expected}/files-summary.md"

  ignore="lib/ndjson-ignore.txt"
  for combo in \
    "pre/voom-db.ndjson:pre/ffprobe.ndjson:db-vs-ffprobe-pre.tsv" \
    "post/voom-db.ndjson:post/ffprobe.ndjson:db-vs-ffprobe-post.tsv" \
    "pre/voom-db.ndjson:post/voom-db.ndjson:voom-db-pre-vs-post.tsv" \
    "pre/ffprobe.ndjson:post/ffprobe.ndjson:ffprobe-pre-vs-post.tsv"; do
    IFS=: read -r left right out <<<"${combo}"
    "lib/diff-ndjson.py" \
      "tests/fixtures/${scenario}/${left}" \
      "tests/fixtures/${scenario}/${right}" \
      "${actual}/${out}" \
      --ignore-file "${ignore}"
    assert_match "${actual}/${out}" "${expected}/${out}"
  done

  "lib/codec-pivot.py" \
    "tests/fixtures/${scenario}/pre/ffprobe.ndjson" \
    "tests/fixtures/${scenario}/post/ffprobe.ndjson" \
    "${actual}/codec-pivot.md"
  assert_match "${actual}/codec-pivot.md" "${expected}/codec-pivot.md"

  "lib/tracks-pivot.py" \
    "tests/fixtures/${scenario}/pre/ffprobe.ndjson" \
    "tests/fixtures/${scenario}/post/ffprobe.ndjson" \
    "${actual}/tracks-pivot.md"
  assert_match "${actual}/tracks-pivot.md" "${expected}/tracks-pivot.md"

  rm -rf "${actual}"
  trap - EXIT
done

if ((fail)); then
  echo "TESTS FAILED" >&2
  exit 1
fi
echo "TESTS PASSED"
