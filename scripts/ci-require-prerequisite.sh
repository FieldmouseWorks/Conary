#!/usr/bin/env bash
# scripts/ci-require-prerequisite.sh
# Fail a gate job with an actionable annotation when a prerequisite job did not succeed.
set -euo pipefail

usage() {
  echo "usage: $0 JOB_ID RESULT" >&2
  echo "  JOB_ID: prerequisite job id (^[a-z0-9][a-z0-9-]*\$)" >&2
  echo "  RESULT: success | failure | cancelled | skipped" >&2
}

if [[ "$#" -ne 2 ]]; then
  usage
  exit 2
fi

job_id="$1"
result="$2"

if [[ ! "$job_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
  echo "invalid prerequisite job id: $job_id" >&2
  usage
  exit 2
fi

case "$result" in
  success | failure | cancelled | skipped) ;;
  *)
    echo "invalid prerequisite job result: $result" >&2
    usage
    exit 2
    ;;
esac

if [[ "$result" == success ]]; then
  echo "prerequisite job $job_id: success"
  exit 0
fi

echo "::error title=Prerequisite job did not succeed::prerequisite job $job_id finished as $result; this job did not run its own checks. Read $job_id's log for the cause." >&2
exit 1
