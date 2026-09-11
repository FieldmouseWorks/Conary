#!/usr/bin/env bash
# scripts/refresh-line-cap-issue-state.sh
#
# Regenerate scripts/line-cap-issue-state.txt from the issues cited by
# scripts/line-cap-allowlist.txt. This is the only line-cap step that may use
# the network; scripts/check-line-cap.sh itself stays hermetic and reads the
# checked-in snapshot.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

allowlist="$repo_root/scripts/line-cap-allowlist.txt"
snapshot="$repo_root/scripts/line-cap-issue-state.txt"

command -v gh >/dev/null 2>&1 || {
    echo "ERROR: gh is required to refresh the line-cap issue-state snapshot" >&2
    exit 1
}
gh auth status >/dev/null 2>&1 || {
    echo "ERROR: gh is not authenticated; run 'gh auth login' before refreshing $snapshot" >&2
    exit 1
}
[[ -f "$allowlist" ]] || {
    echo "ERROR: line-cap allowlist not found: $allowlist" >&2
    exit 1
}

# Citations may repeat; the snapshot records each cited issue once.
data_lines="$(awk '!/^[[:space:]]*#/ && NF > 0 { count++ } END { print count + 0 }' "$allowlist")"
[[ "$data_lines" -gt 0 ]] || {
    echo "ERROR: no issue citations found in $allowlist" >&2
    exit 1
}
mapfile -t citations < <(
    awk '!/^[[:space:]]*#/ && $2 ~ /^#[0-9]+$/ { print substr($2, 2) }' "$allowlist"
)
[[ "${#citations[@]}" -eq "$data_lines" ]] || {
    echo "ERROR: malformed allowlist entry in $allowlist (expected '<path> #<issue>')" >&2
    exit 1
}
mapfile -t issues < <(printf '%s\n' "${citations[@]}" | sort -n -u)

{
    echo "# Issue state for scripts/line-cap-allowlist.txt entries."
    echo "# Regenerate with scripts/refresh-line-cap-issue-state.sh after any allowlist change."
    echo "# refreshed: $(date -u +%Y-%m-%d)"
    echo "#"
    echo "# Format: #<issue> <STATE>"
    echo "# refreshed is the UTC date; the checker fails when the allowlist file is newer than it."
    for issue in "${issues[@]}"; do
        if ! state="$(gh issue view "$issue" --json state -q '.state' 2>&1)"; then
            echo "ERROR: cannot read state for issue #$issue: $state" >&2
            exit 1
        fi
        case "$state" in
            OPEN | CLOSED) printf '%s %s\n' "$issue" "$state" ;;
            *)
                echo "ERROR: issue #$issue returned unexpected state: $state" >&2
                exit 1
                ;;
        esac
    done
} > "$snapshot.tmp"
mv "$snapshot.tmp" "$snapshot"

echo "refreshed $snapshot from $data_lines allowlist citations"
