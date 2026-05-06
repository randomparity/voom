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
    # shellcheck disable=SC2034  # pre/post used by Task 8-11 invocations (stub)
    pre="tests/fixtures/${scenario}/pre"
    # shellcheck disable=SC2034  # pre/post used by Task 8-11 invocations (stub)
    post="tests/fixtures/${scenario}/post"
    expected="tests/expected/${scenario}"
    actual=$(mktemp -d)
    trap 'rm -rf "${actual}"' EXIT

    # Diff scripts will be invoked here once they exist (Tasks 8, 9, 10, 11).
    # Each invocation: write to ${actual}/<artifact>, then assert_match against
    # ${expected}/<artifact>.

    rm -rf "${actual}"
    trap - EXIT
done

if ((fail)); then
    echo "TESTS FAILED" >&2
    exit 1
fi
echo "TESTS PASSED"
