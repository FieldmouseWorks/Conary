#!/usr/bin/env bash
# scripts/check-line-cap.sh
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# A custom scan cannot inherit authority from this repository's snapshot.
# Consume option operands so a path named like an option is never a flag.
scan_args=("$@")
custom_scan=false
explicit_snapshot=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --root|--allowlist|--issue-state)
            [[ $# -ge 2 ]] || { echo "ERROR: $1 requires a path" >&2; exit 2; }
            if [[ "$1" == --issue-state ]]; then
                explicit_snapshot=true
            else
                custom_scan=true
            fi
            shift 2
            ;;
        *) shift ;;
    esac
done
if [[ "$custom_scan" == true && "$explicit_snapshot" == false ]]; then
    echo 'ERROR: overriding --root or --allowlist requires an explicit --issue-state path' >&2
    exit 2
fi

exec cargo run -q -p conary-xtask -- \
    line-cap \
    --root "$repo_root" \
    --allowlist "$repo_root/scripts/line-cap-allowlist.txt" \
    --issue-state "$repo_root/scripts/line-cap-issue-state.txt" \
    "${scan_args[@]}"
