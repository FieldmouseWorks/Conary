#!/usr/bin/env bash
# scripts/test-ci-require-prerequisite.sh
# Self-test for scripts/ci-require-prerequisite.sh.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/ci-require-prerequisite.sh"
failures=0

# expect_exit EXPECTED_CODE STDERR_SUBSTRING... -- ARGS...
check() {
  local want="$1" needle="$2"
  shift 2
  local err code=0
  err="$(bash "$script" "$@" 2>&1 >/dev/null)" || code=$?
  if [[ "$code" -ne "$want" ]]; then
    echo "FAIL: ($*) exit $code, want $want" >&2
    failures=$((failures + 1))
  elif [[ -n "$needle" && "$err" != *"$needle"* ]]; then
    echo "FAIL: ($*) stderr lacks '$needle': $err" >&2
    failures=$((failures + 1))
  fi
}

out="$(bash "$script" gnu-compiler-cache success)"
[[ "$out" == "prerequisite job gnu-compiler-cache: success" ]] || {
  echo "FAIL: success output: $out" >&2
  failures=$((failures + 1))
}

for result in failure cancelled skipped; do
  check 1 "::error title=Prerequisite job did not succeed::prerequisite job ci-base-image-policy finished as $result;" \
    ci-base-image-policy "$result"
  check 1 "Read ci-base-image-policy's log for the cause." ci-base-image-policy "$result"
done

check 2 "invalid prerequisite job id" "Bad_Id" success
check 2 "invalid prerequisite job id" "-lead" success
check 2 "invalid prerequisite job result" job-a "" 
check 2 "invalid prerequisite job result" job-a weird
check 2 "usage:"
check 2 "usage:" job-a
check 2 "usage:" job-a success extra

if [[ "$failures" -ne 0 ]]; then
  echo "$failures check(s) failed" >&2
  exit 1
fi
echo "ci-require-prerequisite: all checks passed"
