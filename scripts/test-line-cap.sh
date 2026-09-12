#!/usr/bin/env bash
# scripts/test-line-cap.sh
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
checker="$repo_root/scripts/check-line-cap.sh"
[[ -x "$checker" ]] || {
    echo "ERROR: scripts/check-line-cap.sh is not executable" >&2
    exit 1
}

fixture_root="$(mktemp -d)"
trap 'rm -rf "$fixture_root"' EXIT
mkdir -p "$fixture_root/crates/fixture/src/tests"
allowlist="$fixture_root/allowlist.txt"
issue_state="$fixture_root/line-cap-issue-state.txt"
: > "$allowlist"

write_lines() {
    local path="$1"
    local count="$2"
    echo "// ${path#"$fixture_root"/}" > "$path"
    awk -v count="$((count - 1))" 'BEGIN { for (i = 1; i <= count; i++) print "// fixture line " i }' >> "$path"
}

write_fixture() {
    local path="$1"
    { echo "// ${path#"$fixture_root"/}"; cat; } > "$path"
}

write_issue_state() {
    local path="$1"
    local refreshed="$2"
    shift 2
    {
        echo "# Issue state for the fixture allowlist."
        echo "# refreshed: $refreshed"
        echo "#"
        echo "# Format: #<issue> <STATE>"
        printf '%s\n' "$@"
        # The binding the gate actually checks: the canonical entries these
        # states were read for.
        echo
        echo "== allowlist"
        if [[ -f "$allowlist" ]]; then
            LC_ALL=C sort -u "$allowlist" | awk 'NF > 0 && $1 !~ /^#/'
        fi
    } > "$path"
}

run_checker() {
    "$checker" \
        --root "$fixture_root" \
        --allowlist "$allowlist" \
        --issue-state "$issue_state" \
        "$@"
}

write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"

write_lines "$fixture_root/crates/fixture/src/at_cap.rs" 1000
write_lines "$fixture_root/crates/fixture/src/inline_tests.rs" 900
{
cat <<'EOF'
#[cfg(test)]
mod tests {
EOF
awk 'BEGIN { for (i = 1; i <= 150; i++) print "    // test line " i }'
echo '}'
} >> "$fixture_root/crates/fixture/src/inline_tests.rs"
{
cat <<'EOF'
#[cfg(test)]
mod tests_at_cap {
EOF
awk 'BEGIN { for (i = 1; i <= 297; i++) print "    // test line " i }'
echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/inline_tests_at_cap.rs"
write_lines "$fixture_root/crates/fixture/src/tests.rs" 1200
write_lines "$fixture_root/crates/fixture/src/tests/helper.rs" 1200
write_fixture "$fixture_root/crates/fixture/src/block_comment_attribute.rs" <<'EOF'
#[cfg(test)]
/* the typed item span crosses this block comment
   without guessing where the module starts */
mod tests {
    const VALUE: usize = 1;
}
EOF
write_fixture "$fixture_root/crates/fixture/src/doc_comment_attribute.rs" <<'EOF'
#[cfg(test)]
/// A test-only helper.
fn helper() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/all_test_predicate.rs" <<'EOF'
#[cfg(all(test, feature = "fixture"))]
const FIXTURE: &str = "fixture";
EOF
write_fixture "$fixture_root/crates/fixture/src/not_test_predicate.rs" <<'EOF'
#[cfg(not(test))]
fn production_when_not_testing() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/any_test_predicate.rs" <<'EOF'
#[cfg(any(test, feature = "fixture"))]
fn production_with_feature() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/standalone_tests.rs" <<'EOF'
#[test]
fn standalone() {}
#[tokio::test]
async fn asynchronous() {}
#[cfg_attr(test, test)]
fn conditional() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/inner_cfg_test.rs" <<'EOF'
#![cfg(test)]
fn helper() {}
fn other() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/enclosing_cfg.rs" <<'EOF'
#[cfg(any(test, feature = "prod"))]
mod mixed {
    #[cfg(not(feature = "prod"))]
    fn test_only_child() {}
    fn production_child() {}
}
EOF
write_fixture "$fixture_root/crates/fixture/src/cfg_attr_gating.rs" <<'EOF'
#[cfg_attr(all(), cfg(test))]
fn unconditional_test() {}
#[cfg_attr(all(), cfg_attr(feature = "x", cfg(test)))]
fn conditional_production() {}
EOF
cat <<'EOF' > "$fixture_root/crates/fixture/src/correct_path_header.rs"
// crates/fixture/src/correct_path_header.rs
fn production() {}
EOF

run_checker >/dev/null
report="$(run_checker --report)"
grep -q $'block_comment_attribute.rs\ttotal=7\tproduction=1\tinline_test=6' <<<"$report"
grep -q $'doc_comment_attribute.rs\ttotal=4\tproduction=1\tinline_test=3' <<<"$report"
grep -q $'all_test_predicate.rs\ttotal=3\tproduction=1\tinline_test=2' <<<"$report"
grep -q $'not_test_predicate.rs\ttotal=3\tproduction=3\tinline_test=0' <<<"$report"
grep -q $'any_test_predicate.rs\ttotal=3\tproduction=3\tinline_test=0' <<<"$report"
grep -q $'standalone_tests.rs\ttotal=7\tproduction=3\tinline_test=4' <<<"$report"
grep -q $'inner_cfg_test.rs\ttotal=4\tproduction=0\tinline_test=4' <<<"$report"
grep -q $'enclosing_cfg.rs\ttotal=7\tproduction=5\tinline_test=2' <<<"$report"
grep -q $'cfg_attr_gating.rs\ttotal=5\tproduction=3\tinline_test=2' <<<"$report"

for header_kind in missing legacy; do
    header_path="$fixture_root/crates/fixture/src/invalid_header.rs"
    if [[ "$header_kind" == legacy ]]; then
        echo '// fixture/src/invalid_header.rs' > "$header_path"
    else
        : > "$header_path"
    fi
    echo 'fn production() {}' >> "$header_path"
    if run_checker >"$fixture_root/invalid-header.out" 2>&1; then
        echo "ERROR: $header_kind path header unexpectedly passed" >&2
        exit 1
    fi
    grep -Fq "expected \`// crates/fixture/src/invalid_header.rs\`" "$fixture_root/invalid-header.out"
    rm "$header_path"
done

cat <<'EOF' > "$fixture_root/crates/fixture/src/wrong_path_header.rs"
// crates/wrong/src/wrong_path_header.rs
fn production() {}
EOF
if run_checker >"$fixture_root/path-header.out" 2>&1; then
    echo "ERROR: mismatched Rust path header unexpectedly passed" >&2
    exit 1
fi
grep -Fq "expected \`// crates/fixture/src/wrong_path_header.rs\`" "$fixture_root/path-header.out"
rm "$fixture_root/crates/fixture/src/wrong_path_header.rs"

{
awk 'BEGIN { for (i = 1; i <= 399; i++) print "// fixture line " i }'
cat <<'EOF'
#[cfg(test)]
mod middle_tests {
    const OPEN_BRACE: &str = "{";
    // }
}
EOF
awk 'BEGIN { for (i = 1; i <= 300; i++) print "// middle production line " i }'
cat <<'EOF'
#[cfg(test)] mod later_tests {
    const CLOSE_BRACE: &str = r#"}"#;
}
EOF
awk 'BEGIN { for (i = 1; i <= 301; i++) print "// trailing production line " i }'
} | write_fixture "$fixture_root/crates/fixture/src/production_after_inline.rs"
if run_checker >"$fixture_root/after-inline.out" 2>&1; then
    echo "ERROR: production after an inline test module was not counted" >&2
    exit 1
fi
grep -q 'production_after_inline.rs has 1001 non-test lines' "$fixture_root/after-inline.out"
rm "$fixture_root/crates/fixture/src/production_after_inline.rs"

write_lines "$fixture_root/crates/fixture/src/over_cap.rs" 1001
if run_checker >"$fixture_root/over.out" 2>&1; then
    echo "ERROR: unallowlisted over-cap fixture unexpectedly passed" >&2
    exit 1
fi
grep -q 'over_cap.rs has 1001 non-test lines' "$fixture_root/over.out"

echo 'crates/fixture/src/over_cap.rs #0' > "$allowlist"
if run_checker >"$fixture_root/nonpositive-issue.out" 2>&1; then
    echo "ERROR: non-positive allowlist issue unexpectedly passed" >&2
    exit 1
fi
grep -q "invalid allowlist entry" "$fixture_root/nonpositive-issue.out"

echo 'crates/fixture/src/over_cap.rs #123' > "$allowlist"
# The snapshot is bound to the entries it was generated for, so regenerate it
# whenever the allowlist changes. This call is the binding's happy path.
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
allowlisted_out="$(run_checker)"
grep -q 'ALLOWLISTED: crates/fixture/src/over_cap.rs .* issue=#123' <<<"$allowlisted_out"

closed_issue_state="$fixture_root/closed-issue-state.txt"
write_issue_state "$closed_issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN" "814 CLOSED"
echo 'crates/fixture/src/over_cap.rs #814' > "$allowlist"
if run_checker --issue-state "$closed_issue_state" >"$fixture_root/closed-issue.out" 2>&1; then
    echo "ERROR: allowlist entry citing a closed issue unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'allowlist entry crates/fixture/src/over_cap.rs cites #814' "$fixture_root/closed-issue.out"
grep -Fq 'snapshot records #814 as CLOSED' "$fixture_root/closed-issue.out"

# A citation missing from the snapshot's *states* is its own failure, distinct
# from a binding mismatch: bind the entry first, then leave the state out.
echo 'crates/fixture/src/over_cap.rs #999' > "$allowlist"
rm -f "$issue_state"
awk '!/^== allowlist$/{print} /^== allowlist$/{print; print "crates/fixture/src/over_cap.rs #999"; skip=1; next} skip{next}' "$allowlist" >/dev/null 2>&1 || true
{
    echo "# Issue state for the fixture allowlist."
    echo "# refreshed: $(date -u +%Y-%m-%d)"
    echo "#"
    echo "# Format: #<issue> <STATE>"
    echo "123 OPEN"
    echo
    echo "== allowlist"
    echo "crates/fixture/src/over_cap.rs #999"
} > "$issue_state"
if run_checker >"$fixture_root/missing-issue.out" 2>&1; then
    echo "ERROR: allowlist entry absent from the snapshot unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'allowlist entry crates/fixture/src/over_cap.rs cites #999' "$fixture_root/missing-issue.out"
grep -Fq 'absent from issue-state snapshot' "$fixture_root/missing-issue.out"

echo 'crates/fixture/src/over_cap.rs #123' > "$allowlist"
# Restore the default snapshot so later default-checker calls bind to this
# allowlist; the missing-issue case above deliberately left it bound to #999.
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
run_checker >/dev/null
binding_issue_state="$fixture_root/binding-issue-state.txt"

# A snapshot whose recorded entries do not match the allowlist is stale, no
# matter what the file timestamps say. This is the case the mtime rule missed:
# an allowlist edited on the same UTC day as the last refresh.
write_issue_state "$binding_issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
echo 'crates/fixture/src/other.rs #123' >> "$allowlist"
if run_checker --issue-state "$binding_issue_state" >"$fixture_root/binding-mismatch.out" 2>&1; then
    echo "ERROR: an allowlist the snapshot does not record unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'does not match issue-state snapshot' "$fixture_root/binding-mismatch.out"
grep -Fq 'not recorded: crates/fixture/src/other.rs #123' "$fixture_root/binding-mismatch.out"
grep -Fq 'scripts/refresh-line-cap-issue-state.sh' "$fixture_root/binding-mismatch.out"
echo 'crates/fixture/src/over_cap.rs #123' > "$allowlist"

# A snapshot recording an entry the allowlist no longer cites is stale too, so
# a removed exception cannot leave a stale binding behind.
write_issue_state "$binding_issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
# The snapshot binds only the first entry; citing an extra one afterwards makes
# the recorded and live sets differ in both directions.
printf 'crates/fixture/src/over_cap.rs #123\ncrates/fixture/src/gone.rs #123\n' > "$allowlist"
write_issue_state "$binding_issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
echo 'crates/fixture/src/over_cap.rs #123' > "$allowlist"
if run_checker --issue-state "$binding_issue_state" >"$fixture_root/binding-extra.out" 2>&1; then
    echo "ERROR: a snapshot recording an uncited entry unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'recorded but no longer cited' "$fixture_root/binding-extra.out"

# A fresh checkout stamps every file with the checkout time, so neither file is
# distinguishable by mtime. A matching binding must pass in either ordering,
# and a `git restore` of unchanged bytes must not fail the gate. Both
# orderings are driven here; the earlier mtime rule failed the first one.
write_issue_state "$binding_issue_state" "1970-01-01" "123 OPEN"
for ordering in allowlist-newer snapshot-newer; do
    if [[ "$ordering" == allowlist-newer ]]; then
        touch -d '2000-01-01 00:00:00' "$binding_issue_state"
        touch "$allowlist"
    else
        touch "$allowlist"
        touch -d '2000-01-01 00:00:00' "$binding_issue_state"
    fi
    run_checker --issue-state "$binding_issue_state" >"$fixture_root/binding-order.out" 2>&1 || {
        cat "$fixture_root/binding-order.out" >&2
        echo "ERROR: a matching binding was rejected with $ordering timestamps" >&2
        exit 1
    }
done

run_checker >/dev/null

rm "$fixture_root/crates/fixture/src/over_cap.rs"
if run_checker >"$fixture_root/stale.out" 2>&1; then
    echo "ERROR: stale allowlist fixture unexpectedly passed" >&2
    exit 1
fi
grep -q 'stale line-cap allowlist entry' "$fixture_root/stale.out"

: > "$allowlist"
{
cat <<'EOF'
#[cfg(test)]
mod oversized_tests {
EOF
awk 'BEGIN { for (i = 1; i <= 298; i++) print "    // test line " i }'
echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/oversized_inline_tests.rs"
if run_checker >"$fixture_root/inline-size.out" 2>&1; then
    echo "ERROR: oversized inline test module unexpectedly passed" >&2
    exit 1
fi
grep -q 'oversized_inline_tests.rs has 301 inline test lines' "$fixture_root/inline-size.out"

echo 'crates/fixture/src/oversized_inline_tests.rs #123' > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
run_checker >/dev/null
rm "$fixture_root/crates/fixture/src/oversized_inline_tests.rs"

: > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
{
cat <<'EOF'
#[cfg(test)]
mod first_tests {
EOF
awk 'BEGIN { for (i = 1; i <= 148; i++) print "    // first test line " i }'
echo '}'
cat <<'EOF'
#[cfg(test)]
mod second_tests {
EOF
awk 'BEGIN { for (i = 1; i <= 148; i++) print "    // second test line " i }'
echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/multiple_inline_tests.rs"
if run_checker >"$fixture_root/multiple-inline.out" 2>&1; then
    echo "ERROR: multiple inline test regions were not summed" >&2
    exit 1
fi
grep -q 'multiple_inline_tests.rs has 302 inline test lines' "$fixture_root/multiple-inline.out"
rm "$fixture_root/crates/fixture/src/multiple_inline_tests.rs"

{
    echo '#[test]'
    echo 'fn standalone() {}'
    echo '#[cfg(test)]'
    echo 'mod tests {'
    awk 'BEGIN { for (i = 1; i <= 296; i++) print "    // test line " i }'
    echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/mixed_inline_tests.rs"
if run_checker >"$fixture_root/mixed-inline.out" 2>&1; then
    echo "ERROR: standalone and cfg-gated tests were not summed" >&2
    exit 1
fi
grep -q 'mixed_inline_tests.rs has 301 inline test lines' "$fixture_root/mixed-inline.out"
rm "$fixture_root/crates/fixture/src/mixed_inline_tests.rs"

: > "$allowlist"
{
cat <<'EOF'
#[cfg(test)]
mod tests;
EOF
awk 'BEGIN { for (i = 1; i <= 1000; i++) print "// production line " i }'
} | write_fixture "$fixture_root/crates/fixture/src/external_tests.rs"
if run_checker >"$fixture_root/external.out" 2>&1; then
    echo "ERROR: external test declaration hid trailing production lines" >&2
    exit 1
fi
grep -q 'external_tests.rs has 1001 non-test lines' "$fixture_root/external.out"

rm "$fixture_root/crates/fixture/src/external_tests.rs"

{
    echo '#[cfg(any(test, not(unix)))]'
    echo 'fn production_on_other_platforms() {'
    awk 'BEGIN { for (i = 1; i <= 997; i++) print "    // production" }'
    echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/platform_production.rs"
if run_checker >"$fixture_root/platform.out" 2>&1; then
    echo "ERROR: negated platform predicate hid production lines" >&2
    exit 1
fi
grep -q 'platform_production.rs has 1001 non-test lines' "$fixture_root/platform.out"
rm "$fixture_root/crates/fixture/src/platform_production.rs"

{
    echo 'fn production() {'
    echo '    #[cfg(test)]'
    echo '    {'
    awk 'BEGIN { for (i = 1; i <= 298; i++) print "        // test block" }'
    echo '    }'
    echo '}'
} | write_fixture "$fixture_root/crates/fixture/src/test_block.rs"
if run_checker >"$fixture_root/test-block.out" 2>&1; then
    echo "ERROR: oversized test-only block escaped the inline cap" >&2
    exit 1
fi
grep -q 'test_block.rs has 301 inline test lines' "$fixture_root/test-block.out"
rm "$fixture_root/crates/fixture/src/test_block.rs"

# A top-level Rust directory that no policy declares must fail the gate.
mkdir -p "$fixture_root/fixture_pkg/src"
write_fixture "$fixture_root/fixture_pkg/src/undeclared.rs" <<'EOF'
fn undeclared() {}
EOF
if run_checker >"$fixture_root/undeclared.out" 2>&1; then
    echo "ERROR: undeclared top-level Rust source root unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'undeclared top-level Rust source root(s)' "$fixture_root/undeclared.out"
grep -Fq 'fixture_pkg' "$fixture_root/undeclared.out"

# The classification error is keyed to the declared policy, so the same Rust
# measured from a declared root no longer fails for that reason.
mv "$fixture_root/fixture_pkg" "$fixture_root/crates/fixture_pkg"
write_fixture "$fixture_root/crates/fixture_pkg/src/undeclared.rs" <<'EOF'
fn undeclared() {}
EOF
declared_report="$(run_checker --report)"
if grep -Fq 'undeclared top-level Rust source root' <<<"$declared_report"; then
    echo "ERROR: declared source root was reported as undeclared" >&2
    exit 1
fi
grep -q $'crates/fixture_pkg/src/undeclared.rs\ttotal=2\tproduction=2\tinline_test=0' <<<"$declared_report"
rm -rf "$fixture_root/crates/fixture_pkg"

# A declared-but-vendor-excluded root is counted and never measured: its
# over-cap file and its stale path comment cannot fail the gate.
mkdir -p "$fixture_root/third_party/vendored/src"
write_lines "$fixture_root/third_party/vendored/src/over_cap.rs" 1001
{
    echo '// vendor/src/legacy_header.rs'
    echo 'fn vendored() {}'
} > "$fixture_root/third_party/vendored/src/legacy_header.rs"
vendor_report="$(run_checker --report)"
grep -q 'SOURCE ROOTS: apps=0 files (scanned); crates=[0-9]* files (scanned); third_party=2 files (vendor-excluded: ' <<<"$vendor_report"
for vendor_file in over_cap.rs legacy_header.rs; do
    if grep -Fq "third_party/vendored/src/$vendor_file" <<<"$vendor_report"; then
        echo "ERROR: vendor-excluded $vendor_file was reported as measured" >&2
        exit 1
    fi
done
if grep -q 'SOURCE ROOTS' <<<"$(run_checker)"; then
    echo "ERROR: coverage statement leaked outside --report" >&2
    exit 1
fi

# Exemption classification (issue #997). Every exempt-named file is reported and
# classified instead of being silently dropped, and the classification is
# report-only: it must never move the exit code. The gates below cover an inner
# `#![cfg(test)]`, a `#[cfg(test)]` declaring site, a production declaring site,
# a cargo integration-test target and a file no declaration reaches.
rm -f "$fixture_root/crates/fixture/src/tests.rs"
rm -rf "$fixture_root/crates/fixture/src/tests"
mkdir -p "$fixture_root/crates/fixture/src/gated_small" \
    "$fixture_root/crates/fixture/src/gated_large" \
    "$fixture_root/crates/fixture/src/ungated_large" \
    "$fixture_root/crates/fixture/src/orphan" \
    "$fixture_root/crates/fixture/src/tests" \
    "$fixture_root/crates/fixture/tests"
write_fixture "$fixture_root/crates/fixture/src/lib.rs" <<'EOF'
mod gated_small;
mod gated_large;
mod ungated_large;
#[path = "../tests/shared.rs"]
pub mod shared;
EOF
write_fixture "$fixture_root/crates/fixture/src/gated_small.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/gated_small/tests.rs" 40
write_fixture "$fixture_root/crates/fixture/src/gated_large.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/gated_large/tests.rs" 1200
write_fixture "$fixture_root/crates/fixture/src/ungated_large.rs" <<'EOF'
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/ungated_large/tests.rs" 1200
write_fixture "$fixture_root/crates/fixture/src/tests/inner_gate.rs" <<'EOF'
#![cfg(test)]
fn helper() {}
EOF
write_fixture "$fixture_root/crates/fixture/src/orphan/tests.rs" <<'EOF'
fn helper() {}
EOF
write_fixture "$fixture_root/crates/fixture/tests/integration.rs" <<'EOF'
fn integration() {}
EOF
write_fixture "$fixture_root/crates/fixture/tests/shared.rs" <<'EOF'
pub fn helper() {}
EOF

: > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
exempt_report="$(run_checker --report)"
grep -q $'EXEMPT: crates/fixture/src/gated_small/tests.rs\ttotal=40\tproduction=40\tinline_test=0\tgate=test-gated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/gated_large/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0\tgate=test-gated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/ungated_large/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0\tgate=ungated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/tests/inner_gate.rs\ttotal=3\tproduction=0\tinline_test=3\tgate=test-gated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/orphan/tests.rs\ttotal=2\tproduction=2\tinline_test=0\tgate=unknown' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/tests/integration.rs\ttotal=2\tproduction=2\tinline_test=0\tgate=test-gated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/tests/shared.rs\ttotal=2\tproduction=2\tinline_test=0\tgate=ungated' <<<"$exempt_report"
grep -q 'EXEMPT SUMMARY: test-gated=4 ungated=2 unknown=1' <<<"$exempt_report"
# An exempt-named file never gains an ordinary row, whatever its gate.
if grep -q $'^crates/fixture/src/ungated_large/tests.rs\t' <<<"$exempt_report"; then
    echo "ERROR: exempt-named file gained an ordinary report row" >&2
    exit 1
fi

# The allowlist stale-entry rule for exempt-named files (#997 correction). An
# exception counts as used only when it excuses a cap violation the gate
# enforces; the gate enforces no cap on a file whose name exempts it, so a
# listed exempt-named file is stale however large it measures, and its size
# never produces an error of its own.
write_lines "$fixture_root/crates/fixture/src/control_over_cap.rs" 1001

# Counterfactual for a cap-checked file: the entry is used, and removing it
# restores the exact cap error the entry suppresses.
echo 'crates/fixture/src/control_over_cap.rs #123' > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
used_out="$(run_checker)"
grep -q 'ALLOWLISTED: crates/fixture/src/control_over_cap.rs production=1001 inline_test=0 issue=#123' <<<"$used_out"
if grep -q 'stale line-cap allowlist entry' <<<"$used_out"; then
    echo "ERROR: a cap-checked exception was reported stale" >&2
    exit 1
fi
: > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
if run_checker >"$fixture_root/control-removed.out" 2>&1; then
    echo "ERROR: removing the cap-checked exception restored no cap error" >&2
    exit 1
fi
grep -q 'control_over_cap.rs has 1001 non-test lines' "$fixture_root/control-removed.out"

printf '%s\n' \
    'crates/fixture/src/gated_small/tests.rs #123' \
    'crates/fixture/src/gated_large/tests.rs #123' \
    'crates/fixture/src/ungated_large/tests.rs #123' > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
if run_checker >"$fixture_root/exempt-stale.out" 2>&1; then
    echo "ERROR: a listed exempt-named file was not reported stale" >&2
    exit 1
fi
for exempt_entry in gated_small/tests.rs gated_large/tests.rs ungated_large/tests.rs; do
    grep -q "stale line-cap allowlist entry: crates/fixture/src/$exempt_entry #123" "$fixture_root/exempt-stale.out"
    if grep -q "crates/fixture/src/$exempt_entry has " "$fixture_root/exempt-stale.out"; then
        echo "ERROR: exempt-named $exempt_entry was cap-checked" >&2
        exit 1
    fi
done
: > "$allowlist"
write_issue_state "$issue_state" "$(date -u +%Y-%m-%d)" "123 OPEN"
rm "$fixture_root/crates/fixture/src/control_over_cap.rs"

write_fixture "$fixture_root/crates/fixture/src/malformed.rs" <<'EOF'
fn malformed( {
EOF
if run_checker >"$fixture_root/malformed.out" 2>&1; then
    echo "ERROR: malformed Rust fixture unexpectedly passed" >&2
    exit 1
fi
grep -q 'failed to parse crates/fixture/src/malformed.rs' "$fixture_root/malformed.out"

echo "line-cap tests passed."
