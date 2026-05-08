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
