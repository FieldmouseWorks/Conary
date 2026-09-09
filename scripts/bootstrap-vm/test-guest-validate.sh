#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_SCRIPT="$SCRIPT_DIR/guest-validate.sh"

TMPDIR_ROOT="$(mktemp -d)"
FAKEBIN="$TMPDIR_ROOT/fakebin"
TARGET_SCRIPT="$TMPDIR_ROOT/guest-validate.sh"
INPUTS_DIR="$TMPDIR_ROOT/bootstrap-inputs"
WORKSPACE_SRC="$TMPDIR_ROOT/workspace-src"
CONARY_LOG="$TMPDIR_ROOT/conary.log"
CARGO_LOG="$TMPDIR_ROOT/cargo.log"
DB_INIT_MARKER="$TMPDIR_ROOT/db-initialized"
TEST_REPO_DB="$TMPDIR_ROOT/repositories.db"
FAKE_CONARY="$FAKEBIN/conary"

cleanup() {
    rm -rf "$TMPDIR_ROOT"
}
trap cleanup EXIT

mkdir -p "$FAKEBIN" "$INPUTS_DIR" "$WORKSPACE_SRC/conary-workspace/recipes/bootstrap-smoke"
cp "$SOURCE_SCRIPT" "$TARGET_SCRIPT"

python3 - "$TARGET_SCRIPT" "$INPUTS_DIR" "$TEST_REPO_DB" <<'PY'
from pathlib import Path
import sys

script_path = Path(sys.argv[1])
inputs_dir = sys.argv[2]
text = script_path.read_text()
text = text.replace(
    'INPUTS_DIR="/var/lib/conary/bootstrap-inputs"',
    f'INPUTS_DIR="{inputs_dir}"',
)
text = text.replace("/var/lib/conary/conary.db", sys.argv[3])
script_path.write_text(text)
PY

cat >"$WORKSPACE_SRC/conary-workspace/recipes/bootstrap-smoke/simple-hello.toml" <<'EOF'
name = "simple-hello"
version = "1.0.0"
source = { url = "https://example.invalid/simple-hello-1.0.0.tar.gz", checksum = "sha256:test" }
build = []
package = []
EOF

printf '{"signed": {"type": "root"}}\n' >"$INPUTS_DIR/root.json"

tar -czf "$INPUTS_DIR/conary-workspace.tar.gz" -C "$WORKSPACE_SRC" conary-workspace
sha256sum "$INPUTS_DIR/conary-workspace.tar.gz" | awk '{print $1}' >"$INPUTS_DIR/conary-workspace.tar.gz.sha256"

cat >"$FAKE_CONARY" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$TEST_CONARY_LOG"

case "${1:-} ${2:-}" in
    "system init")
        : >"$TEST_DB_INIT_MARKER"
        python3 - "$TEST_REPO_DB" "${TEST_SOURCE_STATE:-enabled}" <<'PYDB'
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as db:
    db.execute("CREATE TABLE IF NOT EXISTS repositories (name TEXT, enabled INTEGER)")
    db.execute("DELETE FROM repositories")
    if sys.argv[2] != "missing":
        db.execute("INSERT INTO repositories VALUES (?, ?)", ("remi-fedora-44", int(sys.argv[2] == "enabled")))
PYDB
        exit 0
        ;;
    "repo remove")
        exit 0
        ;;
    "repo list")
        echo "human repository output must not establish onboarding state" >&2
        exit 99
        ;;
    *)
        ;;
esac

if [[ ! -f "$TEST_DB_INIT_MARKER" ]]; then
    echo "Database not initialized." >&2
    exit 42
fi

exit 0
EOF
chmod +x "$FAKE_CONARY"

cat >"$FAKEBIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$TEST_CARGO_LOG"
mkdir -p target/debug
ln -sf "$TEST_FAKE_CONARY" target/debug/conary
exit 0
EOF
chmod +x "$FAKEBIN/cargo"

cat >"$FAKEBIN/sqlite3" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'sqlite3 %s\n' "$*" >>"$TEST_CONARY_LOG"
[[ "$1" == -readonly ]]
python3 - "$2" "$3" <<'PYDB'
import pathlib, sqlite3, sys
with sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri() + "?mode=ro", uri=True) as db:
    for row in db.execute(sys.argv[2]):
        print(row[0])
PYDB
EOF
chmod +x "$FAKEBIN/sqlite3"
export TEST_REPO_DB

export PATH="$FAKEBIN:$PATH"
export TEST_CONARY_LOG="$CONARY_LOG"
export TEST_CARGO_LOG="$CARGO_LOG"
export TEST_DB_INIT_MARKER="$DB_INIT_MARKER"
export TEST_FAKE_CONARY="$FAKE_CONARY"

bash "$TARGET_SCRIPT" \
    --repo-name remi-fedora-44 \
    --repo-url https://remi.conary.io \
    --remi-endpoint https://remi.conary.io \
    --source-profile fedora-44

assert_contains() {
    local file="$1"
    local needle="$2"
    if ! grep -Fq -- "$needle" "$file"; then
        echo "expected '$needle' in $file" >&2
        exit 1
    fi
}

assert_not_contains() {
    local file="$1"
    local needle="$2"
    if grep -Fq -- "$needle" "$file"; then
        echo "did not expect '$needle' in $file" >&2
        exit 1
    fi
}

line_number() {
    local file="$1"
    local needle="$2"
    grep -Fn -- "$needle" "$file" | head -n 1 | cut -d: -f1
}

assert_order() {
    local file="$1"
    local first="$2"
    local second="$3"
    local first_line
    local second_line

    first_line="$(line_number "$file" "$first")"
    second_line="$(line_number "$file" "$second")"

    if [[ -z "$first_line" || -z "$second_line" || "$first_line" -ge "$second_line" ]]; then
        echo "expected '$first' to appear before '$second' in $file" >&2
        exit 1
    fi
}

assert_contains "$CONARY_LOG" "system init"
assert_not_contains "$CONARY_LOG" "repo add"
assert_contains "$CONARY_LOG" "sqlite3 -readonly $TEST_REPO_DB"
assert_not_contains "$CONARY_LOG" "repo list"
assert_contains "$CONARY_LOG" "trust init remi-fedora-44 --root $INPUTS_DIR/root.json"
assert_contains "$CONARY_LOG" "repo sync remi-fedora-44 --force"
assert_contains "$CONARY_LOG" "query label list"
assert_contains "$CONARY_LOG" "cook $INPUTS_DIR/conary-workspace/recipes/bootstrap-smoke/simple-hello.toml --output /var/tmp/conary-smoke-output --source-cache /var/tmp/conary-smoke-cache"

assert_order "$CONARY_LOG" "system init" "sqlite3 -readonly"
assert_order "$CONARY_LOG" "sqlite3 -readonly" "trust init remi-fedora-44"
assert_order "$CONARY_LOG" "trust init remi-fedora-44" "repo sync remi-fedora-44 --force"
assert_order "$CONARY_LOG" "repo sync remi-fedora-44 --force" "query label list"

assert_contains "$CARGO_LOG" "build --locked"
assert_contains "$CONARY_LOG" "--version"

for state in missing disabled; do
    : >"$CONARY_LOG"
    if TEST_SOURCE_STATE="$state" bash "$TARGET_SCRIPT" \
        --repo-name remi-fedora-44 \
        --repo-url https://remi.conary.io \
        --remi-endpoint https://remi.conary.io \
        --source-profile fedora-44 >"$TMPDIR_ROOT/$state.log" 2>&1; then
        echo "guest validation accepted $state repository" >&2
        exit 1
    fi
    assert_contains "$TMPDIR_ROOT/$state.log" "Expected packaged source"
    assert_not_contains "$CONARY_LOG" "repo sync"
done

echo "guest-validate ordering and source-state tests passed"
