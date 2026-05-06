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

    rm -rf "${actual}"
    trap - EXIT
done

if ((fail)); then
    echo "TESTS FAILED" >&2
    exit 1
fi
echo "TESTS PASSED"
