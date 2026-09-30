#!/usr/bin/env bash
# scripts/test-user-journey.sh
#
# Exercise scripts/user-journey.sh without containers or root: a mock conary
# installs a tiny fixture payload, and each negative case flips exactly one
# behaviour so the exact stage reason proves the rule under test.
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if ! command -v jq >/dev/null 2>&1; then
  echo "test-user-journey harness error: jq is required" >&2
  exit 1
fi

journey="scripts/user-journey.sh"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT

failures=0
fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

fixture="$tmp/fixture-tree"
cat >"$fixture" <<'FIXTURE'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  printf 'tree v0.0.0-test\n'
  exit 0
fi
exit 0
FIXTURE
chmod 0755 "$fixture"

wrong="$tmp/wrong-tree"
printf 'not the package payload\n' >"$wrong"

fixture_sha_line="$(sha256sum "$fixture")"
fixture_sha="${fixture_sha_line%% *}"
dummy_sha="0000000000000000000000000000000000000000000000000000000000000000"

mock="$tmp/mock-conary"
cat >"$mock" <<'MOCK'
#!/usr/bin/env bash
# Mock conary for scripts/test-user-journey.sh.
set -u
mode="${MOCK_MODE:-normal}"
bin_root="${JOURNEY_BIN_ROOT:-}"
if [[ -z "$bin_root" ]]; then
  echo "mock conary requires JOURNEY_BIN_ROOT" >&2
  exit 2
fi
bin_path="$bin_root/usr/bin/tree"
case "${1:-}" in
  system)
    case "${2:-}" in
      init) exit 0 ;;
      adopt)
        if [[ "$mode" == "adopt_fail" ]]; then
          printf 'adopt says "no" with a backslash \\ and a\ttab\n' >&2
          exit 3
        fi
        exit 0
        ;;
      *) exit 0 ;;
    esac
    ;;
  install)
    case "$mode" in
      install_noop) exit 0 ;;
      install_wrong)
        mkdir -p "$(dirname "$bin_path")"
        cp "$MOCK_WRONG" "$bin_path"
        chmod 0755 "$bin_path"
        exit 0
        ;;
      *)
        mkdir -p "$(dirname "$bin_path")"
        cp "$MOCK_FIXTURE" "$bin_path"
        chmod 0755 "$bin_path"
        exit 0
        ;;
    esac
    ;;
  remove)
    name="${2:-}"
    if [[ -z "$name" ]]; then
      echo "mock conary remove requires a package name" >&2
      exit 2
    fi
    bin_path="$bin_root/usr/bin/$name"
    case "$mode" in
      remove_leave) exit 0 ;;
      *) rm -f "$bin_path"; exit 0 ;;
    esac
    ;;
  --version)
    printf 'mock-conary 0.0.0\n'
    exit 0
    ;;
  *) exit 0 ;;
esac
MOCK
chmod 0755 "$mock"

tsv="$tmp/packages.tsv"
downloads="$tmp/downloads"
mkdir -p "$downloads"
{
  printf 'profile\tname\tformat\tfile\tsha256\turl\tbinary\tbinary_sha256\n'
  printf 'fedora-44\ttree\trpm\ttree-2.2.1-4.fc44.x86_64.rpm\t%s\thttps://example.invalid/tree.rpm\t/usr/bin/tree\t%s\n' \
    "$dummy_sha" "$fixture_sha"
  printf 'ubuntu-26.04\ttree\tdeb\ttree_2.3.1-1_amd64.deb\t%s\thttps://example.invalid/tree.deb\t/usr/bin/tree\t%s\n' \
    "$dummy_sha" "$fixture_sha"
  printf 'arch\ttree\talpm\ttree-2.3.2-1-x86_64.pkg.tar.zst\t%s\thttps://example.invalid/tree.zst\t/usr/bin/tree\t%s\n' \
    "$dummy_sha" "$fixture_sha"
} >"$tsv"
: >"$downloads/tree-2.2.1-4.fc44.x86_64.rpm"
: >"$downloads/tree_2.3.1-1_amd64.deb"
: >"$downloads/tree-2.3.2-1-x86_64.pkg.tar.zst"

new_root() {
  mktemp -d "$tmp/root.XXXXXX"
}

run_journey() {
  local mode="$1" host="$2" out="$3" root="$4"
  MOCK_MODE="$mode" MOCK_FIXTURE="$fixture" MOCK_WRONG="$wrong" \
    JOURNEY_CONARY="$mock" JOURNEY_BIN_ROOT="$root" \
    bash "$journey" --host "$host" --packages "$tsv" --downloads "$downloads" \
    --evidence "$out"
}

# Positive control: every stage passes with real digests.
positive_root="$(new_root)"
positive_out="$tmp/positive.json"
status=0
run_journey normal fedora-44 "$positive_out" "$positive_root" || status=$?
if [[ "$status" -ne 0 ]]; then
  fail "positive control exited $status"
fi
if ! jq -e '
    .schema == "conary-user-journey-v1" and
    .host == "fedora-44" and
    .conary_version == "mock-conary 0.0.0" and
    (.stages | length == 14) and
    ([.stages[].passed] | all) and
    ([.stages[].reason] | all(. == "ok")) and
    ([.stages[].exit_code] | all(. == 0)) and
    ([.stages[].id] == [
      "init", "adopt",
      "absent-before:fedora-44", "install:fedora-44", "run:fedora-44", "remove:fedora-44",
      "absent-before:ubuntu-26.04", "install:ubuntu-26.04", "run:ubuntu-26.04", "remove:ubuntu-26.04",
      "absent-before:arch", "install:arch", "run:arch", "remove:arch"
    ])
  ' "$positive_out" >/dev/null 2>&1; then
  fail "positive control evidence mismatch"
fi

# A host that is not the first table row must still run first.
ordered_root="$(new_root)"
ordered_out="$tmp/ordered.json"
ordered_status=0
run_journey normal ubuntu-26.04 "$ordered_out" "$ordered_root" || ordered_status=$?
if [[ "$ordered_status" -ne 0 ]]; then
  fail "host-first ordering exited $ordered_status"
fi
if ! jq -e '
    (.stages | length == 14) and
    ([.stages[].passed] | all) and
    ([.stages[].id] == [
      "init", "adopt",
      "absent-before:ubuntu-26.04", "install:ubuntu-26.04", "run:ubuntu-26.04", "remove:ubuntu-26.04",
      "absent-before:fedora-44", "install:fedora-44", "run:fedora-44", "remove:fedora-44",
      "absent-before:arch", "install:arch", "run:arch", "remove:arch"
    ])
  ' "$ordered_out" >/dev/null 2>&1; then
  fail "host-first ordering evidence mismatch"
fi

# Negative cases: each flips exactly one mock behaviour.
check_reason() {
  local mode="$1" stage_id="$2" expected_reason="$3" seed="$4"
  local root out status
  root="$(new_root)"
  if [[ "$seed" == "seed" ]]; then
    mkdir -p "$root/usr/bin"
    cp "$fixture" "$root/usr/bin/tree"
    chmod 0755 "$root/usr/bin/tree"
  fi
  out="$tmp/check-$mode.json"
  status=0
  run_journey "$mode" fedora-44 "$out" "$root" || status=$?
  if [[ "$status" -ne 1 ]]; then
    fail "$mode: expected exit 1, got $status"
    return
  fi
  if ! jq -e --arg id "$stage_id" --arg reason "$expected_reason" \
    '([.stages[] | select(.id == $id) | .reason] == [$reason])' \
    "$out" >/dev/null 2>&1; then
    fail "$mode: expected $stage_id reason $expected_reason"
  fi
}

check_reason install_noop "run:fedora-44" binary_missing no-seed
check_reason install_wrong "run:fedora-44" binary_digest_mismatch no-seed
check_reason remove_leave "remove:fedora-44" still_present no-seed
check_reason preexisting "absent-before:fedora-44" preexisting_binary seed

# adopt failure: exact code, later stages still recorded, and the stderr tail
# survives its double quote, backslash, and tab and still parses as JSON.
adopt_root="$(new_root)"
adopt_out="$tmp/adopt-fail.json"
adopt_status=0
run_journey adopt_fail fedora-44 "$adopt_out" "$adopt_root" || adopt_status=$?
if [[ "$adopt_status" -ne 1 ]]; then
  fail "adopt_fail: expected exit 1, got $adopt_status"
fi
expected_stderr=$'adopt says "no" with a backslash \\ and a\ttab'
if ! jq -e --arg expected "$expected_stderr" \
  '([.stages[] | select(.id == "adopt") | .stderr_tail] == [$expected])' \
  "$adopt_out" >/dev/null 2>&1; then
  fail "adopt_fail: stderr_tail did not round-trip through the JSON escaper"
fi
if ! jq -e '
    ([.stages[] | select(.id == "adopt") | .reason] == ["command_failed"]) and
    ([.stages[] | select(.id == "adopt") | .exit_code] == [3]) and
    (.stages | length == 14) and
    ([.stages[] | select(.id == "run:fedora-44") | .reason] == ["ok"])
  ' "$adopt_out" >/dev/null 2>&1; then
  fail "adopt_fail: expected command_failed with later stages recorded"
fi

if [[ "$failures" -ne 0 ]]; then
  echo "$failures user-journey harness checks failed" >&2
  exit 1
fi
echo "user-journey harness checks passed"
