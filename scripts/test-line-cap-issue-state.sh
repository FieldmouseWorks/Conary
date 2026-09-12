#!/usr/bin/env bash
# scripts/test-line-cap-issue-state.sh
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
fixture_root="$(mktemp -d)"
trap 'rm -rf -- "$fixture_root"' EXIT
git -C "$fixture_root" init -q
mkdir -p "$fixture_root/bin" "$fixture_root/scripts" "$fixture_root/crates/fixture/src"
allowlist="$fixture_root/scripts/line-cap-allowlist.txt"
snapshot="$fixture_root/scripts/line-cap-issue-state.txt"
export LINE_CAP_FIXTURE_CALLS="$fixture_root/gh-calls"
export LINE_CAP_FIXTURE_STATE=OPEN

cat > "$fixture_root/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$LINE_CAP_FIXTURE_CALLS"
case "$*" in
    'auth status --hostname github.com') exit 0 ;;
    'issue view 123 --repo github.com/FieldmouseWorks/Conary --json state -q .state')
        if [[ "$LINE_CAP_FIXTURE_STATE" == unavailable ]]; then
            echo 'fixture issue lookup failed' >&2
            exit 1
        fi
        printf '%s\n' "$LINE_CAP_FIXTURE_STATE"
        ;;
    *) echo "unexpected gh call: $*" >&2; exit 1 ;;
esac
STUB
chmod +x "$fixture_root/bin/gh"

refresh() {
    (
        cd "$fixture_root"
        PATH="$fixture_root/bin:$PATH" GH_REPO=wrong/repository GH_HOST=wrong.invalid \
            bash "$repo_root/scripts/refresh-line-cap-issue-state.sh" "$@"
    )
}

check_snapshot() {
    bash "$repo_root/scripts/check-line-cap.sh" \
        --root "$fixture_root" --allowlist "$allowlist" --issue-state "$snapshot"
}

expect_failure() {
    local message="$1"
    shift
    if "$@" > "$fixture_root/failure.log" 2>&1; then
        echo "ERROR: command unexpectedly succeeded: $*" >&2
        exit 1
    fi
    grep -Fq -- "$message" "$fixture_root/failure.log" || {
        cat "$fixture_root/failure.log" >&2
        echo "ERROR: missing failure evidence: $message" >&2
        exit 1
    }
}

{
    echo '// crates/fixture/src/large.rs'
    awk 'BEGIN { for (i = 0; i < 1000; i++) print "// production fixture" }'
} > "$fixture_root/crates/fixture/src/large.rs"
echo 'crates/fixture/src/large.rs #123' > "$allowlist"

# Exercise the real producer and actual gate with hostile caller defaults.
refresh > "$fixture_root/refresh.log"
check_snapshot > "$fixture_root/open.log"
grep -Fq 'ALLOWLISTED: crates/fixture/src/large.rs' "$fixture_root/open.log"
cp "$snapshot" "$fixture_root/original-snapshot"
refresh --check > "$fixture_root/live-open.log"
cmp "$snapshot" "$fixture_root/original-snapshot"

# Closing a live issue fails while the unchanged snapshot still says OPEN.
export LINE_CAP_FIXTURE_STATE=CLOSED
expect_failure 'crates/fixture/src/large.rs cites #123; github.com/FieldmouseWorks/Conary records #123 as CLOSED' refresh --check
cmp "$snapshot" "$fixture_root/original-snapshot"
refresh > "$fixture_root/closed-refresh.log"
expect_failure 'snapshot records #123 as CLOSED' check_snapshot

# Failed lookups preserve the previous snapshot and clean their temporary file.
cp "$snapshot" "$fixture_root/closed-snapshot"
for state in unavailable invalid; do
    export LINE_CAP_FIXTURE_STATE="$state"
    if [[ "$state" == unavailable ]]; then
        failure='cannot read state for issue #123'
    else
        failure='returned unexpected state'
    fi
    expect_failure "$failure" refresh
    cmp "$snapshot" "$fixture_root/closed-snapshot"
    expect_failure "$failure" refresh --check
    cmp "$snapshot" "$fixture_root/closed-snapshot"
done
if compgen -G "$snapshot.tmp.*" > /dev/null; then
    echo 'ERROR: refresh left temporary snapshot files' >&2
    exit 1
fi

export LINE_CAP_FIXTURE_STATE=OPEN
echo 'crates/fixture/src/large.rs #0' > "$allowlist"
expect_failure 'malformed allowlist entry' refresh
echo 'crates/fixture/src/missing.rs #123' > "$allowlist"
expect_failure 'names a missing file' refresh
printf '%s\n' 'crates/fixture/src/large.rs #123' 'crates/fixture/src/large.rs #123' > "$allowlist"
expect_failure 'duplicate allowlist path' refresh
expect_failure 'Usage:' refresh --unknown
cmp "$snapshot" "$fixture_root/closed-snapshot"

# Removing the final exception leaves a valid empty snapshot and live check.
: > "$allowlist"
echo '// crates/fixture/src/large.rs' > "$fixture_root/crates/fixture/src/large.rs"
refresh > "$fixture_root/empty-refresh.log"
check_snapshot > "$fixture_root/empty-check.log"
refresh --check > "$fixture_root/empty-live.log"
grep -Fq '0 citations' "$fixture_root/empty-live.log"
echo 'line-cap issue-state tests passed.'
