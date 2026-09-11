#!/usr/bin/env bash
# scripts/refresh-line-cap-issue-state.sh
#
# Regenerate scripts/line-cap-issue-state.txt from the issues cited by
# scripts/line-cap-allowlist.txt. This is the only line-cap step that may use
# the network; scripts/check-line-cap.sh itself stays hermetic and reads the
# checked-in snapshot.
#
# The snapshot records two things: the state of each cited issue, and the
# canonical allowlist entries those states were read for. The recorded entry
# set is what the gate compares against, so an allowlist edit cannot pass
# unnoticed and an unchanged allowlist cannot fail because a file was
# rewritten by a checkout or a restore.
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

# Every data line must be a well-formed `<path> #<issue>` citation. An empty
# allowlist is valid: removing the last exception must still be able to refresh
# the snapshot, or finishing the cleanup would break the cleanup machinery.
citations=()
while IFS= read -r entry; do
    [[ -n "$entry" ]] || continue
    read -r path issue extra <<<"$entry"
    if [[ -n "${extra:-}" || -z "${issue:-}" || ! "$issue" =~ ^#[0-9]+$ ]]; then
        echo "ERROR: malformed allowlist entry in $allowlist (expected '<path> #<issue>'): $entry" >&2
        exit 1
    fi
    if [[ ! -f "$repo_root/$path" ]]; then
        echo "ERROR: allowlist entry names a missing file: $path" >&2
        exit 1
    fi
    citations+=("$path $issue")
done < <(awk '!/^[[:space:]]*#/ && NF > 0 { print }' "$allowlist")

# The binding is the canonical entry set: sorted, unique `<path> #<issue>`.
binding=()
issues=()
if [[ ${#citations[@]} -gt 0 ]]; then
    while IFS= read -r line; do
        [[ -n "$line" ]] && binding+=("$line")
    done < <(printf '%s\n' "${citations[@]}" | LC_ALL=C sort -u)
    while IFS= read -r issue; do
        [[ -n "$issue" ]] && issues+=("$issue")
    done < <(printf '%s\n' "${citations[@]}" | awk '{ print substr($2, 2) }' | sort -n -u)
fi

{
    echo "# Issue state for scripts/line-cap-allowlist.txt entries."
    echo "# Regenerate with scripts/refresh-line-cap-issue-state.sh after any allowlist change."
    echo "# refreshed: $(date -u +%Y-%m-%d)"
    echo "#"
    echo "# Format: #<issue> <STATE>"
    echo "# The '== allowlist' section records the entries these states were read for;"
    echo "# it is the binding the gate checks, so edit the allowlist and refresh together."
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
    echo
    echo "== allowlist"
    for entry in "${binding[@]}"; do
        printf '%s\n' "$entry"
    done
} > "$snapshot.tmp"
mv "$snapshot.tmp" "$snapshot"

echo "refreshed $snapshot from ${#binding[@]} allowlist citations"
