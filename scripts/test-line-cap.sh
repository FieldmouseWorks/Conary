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

"$checker" --root "$fixture_root" --allowlist "$allowlist" >/dev/null
report="$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report)"
grep -q $'block_comment_attribute.rs\ttotal=7\tproduction=1\tinline_test=6' <<<"$report"
grep -q $'doc_comment_attribute.rs\ttotal=4\tproduction=1\tinline_test=3' <<<"$report"
grep -q $'all_test_predicate.rs\ttotal=3\tproduction=1\tinline_test=2' <<<"$report"
grep -q $'not_test_predicate.rs\ttotal=3\tproduction=3\tinline_test=0' <<<"$report"
grep -q $'any_test_predicate.rs\ttotal=3\tproduction=3\tinline_test=0' <<<"$report"
grep -q $'standalone_tests.rs\ttotal=7\tproduction=3\tinline_test=4' <<<"$report"
grep -q $'inner_cfg_test.rs\ttotal=4\tproduction=0\tinline_test=4' <<<"$report"
grep -q $'enclosing_cfg.rs\ttotal=7\tproduction=5\tinline_test=2' <<<"$report"
grep -q $'cfg_attr_gating.rs\ttotal=5\tproduction=3\tinline_test=2' <<<"$report"
# A file whose declarations all stay inline keeps the original row shape.
if grep -q $'block_comment_attribute.rs\ttotal=7\tproduction=1\tinline_test=6\tsiblings=' <<<"$report"; then
    echo "ERROR: sibling fields appeared without a resolved out-of-line module" >&2
    exit 1
fi

for header_kind in missing legacy; do
    header_path="$fixture_root/crates/fixture/src/invalid_header.rs"
    if [[ "$header_kind" == legacy ]]; then
        echo '// fixture/src/invalid_header.rs' > "$header_path"
    else
        : > "$header_path"
    fi
    echo 'fn production() {}' >> "$header_path"
    if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/invalid-header.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/path-header.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/after-inline.out" 2>&1; then
    echo "ERROR: production after an inline test module was not counted" >&2
    exit 1
fi
grep -q 'production_after_inline.rs has 1001 non-test lines' "$fixture_root/after-inline.out"
rm "$fixture_root/crates/fixture/src/production_after_inline.rs"

write_lines "$fixture_root/crates/fixture/src/over_cap.rs" 1001
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/over.out" 2>&1; then
    echo "ERROR: unallowlisted over-cap fixture unexpectedly passed" >&2
    exit 1
fi
grep -q 'over_cap.rs has 1001 non-test lines' "$fixture_root/over.out"

echo 'crates/fixture/src/over_cap.rs #0' > "$allowlist"
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/nonpositive-issue.out" 2>&1; then
    echo "ERROR: non-positive allowlist issue unexpectedly passed" >&2
    exit 1
fi
grep -q "invalid allowlist entry" "$fixture_root/nonpositive-issue.out"

echo 'crates/fixture/src/over_cap.rs #123' > "$allowlist"
allowlisted_out="$("$checker" --root "$fixture_root" --allowlist "$allowlist")"
grep -q 'ALLOWLISTED: crates/fixture/src/over_cap.rs .* issue=#123' <<<"$allowlisted_out"

rm "$fixture_root/crates/fixture/src/over_cap.rs"
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/stale.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/inline-size.out" 2>&1; then
    echo "ERROR: oversized inline test module unexpectedly passed" >&2
    exit 1
fi
grep -q 'oversized_inline_tests.rs has 301 inline test lines' "$fixture_root/inline-size.out"

echo 'crates/fixture/src/oversized_inline_tests.rs #123' > "$allowlist"
"$checker" --root "$fixture_root" --allowlist "$allowlist" >/dev/null
rm "$fixture_root/crates/fixture/src/oversized_inline_tests.rs"

: > "$allowlist"
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/multiple-inline.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/mixed-inline.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/external.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/platform.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/test-block.out" 2>&1; then
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
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/undeclared.out" 2>&1; then
    echo "ERROR: undeclared top-level Rust source root unexpectedly passed" >&2
    exit 1
fi
grep -Fq 'undeclared top-level Rust source root(s)' "$fixture_root/undeclared.out"
grep -Fq 'fixture_pkg' "$fixture_root/undeclared.out"

# The classification error is keyed to the declared root names in SOURCE_ROOTS,
# which is compile-time policy: the same Rust under a declared top-level name
# clears the error and is measured from there.
mv "$fixture_root/fixture_pkg" "$fixture_root/apps"
write_fixture "$fixture_root/apps/src/undeclared.rs" <<'EOF'
fn undeclared() {}
EOF
declared_report="$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report)"
if grep -Fq 'undeclared top-level Rust source root' <<<"$declared_report"; then
    echo "ERROR: declared source root was reported as undeclared" >&2
    exit 1
fi
grep -q 'SOURCE ROOTS: apps=1 files (scanned);' <<<"$declared_report"
grep -q $'apps/src/undeclared.rs\ttotal=2\tproduction=2\tinline_test=0' <<<"$declared_report"
rm -rf "$fixture_root/apps"

# A declared-but-vendor-excluded root is counted and never measured: its
# over-cap file and its stale path comment cannot fail the gate.
mkdir -p "$fixture_root/third_party/vendored/src"
write_lines "$fixture_root/third_party/vendored/src/over_cap.rs" 1001
{
    echo '// vendor/src/legacy_header.rs'
    echo 'fn vendored() {}'
} > "$fixture_root/third_party/vendored/src/legacy_header.rs"
vendor_report="$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report)"
grep -q 'SOURCE ROOTS: apps=0 files (scanned); crates=[0-9]* files (scanned); third_party=2 files (vendor-excluded: ' <<<"$vendor_report"
for vendor_file in over_cap.rs legacy_header.rs; do
    if grep -Fq "third_party/vendored/src/$vendor_file" <<<"$vendor_report"; then
        echo "ERROR: vendor-excluded $vendor_file was reported as measured" >&2
        exit 1
    fi
done
if grep -q 'SOURCE ROOTS' <<<"$("$checker" --root "$fixture_root" --allowlist "$allowlist")"; then
    echo "ERROR: coverage statement leaked outside --report" >&2
    exit 1
fi

# Extracted sibling attribution (issue #998). The exempt-named siblings below
# are deliberately over the production cap, so the gate passing here is itself
# the proof that making them visible did not un-exempt them.
: > "$allowlist"
mkdir -p "$fixture_root/crates/fixture/src/path_sibling"
mkdir -p "$fixture_root/crates/fixture/src/mod_parent"

# (a) `#[path = "..."] mod tests;` under a non-`mod.rs` parent, the shape
# apps/conary-test/src/engine/qemu.rs uses.
write_fixture "$fixture_root/crates/fixture/src/path_sibling.rs" <<'EOF'
#[cfg(test)]
#[path = "path_sibling/tests.rs"]
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/path_sibling/tests.rs" 1200

# (b) a plain `mod tests;` under a `mod.rs` parent, the shape
# crates/conary-core/src/repository/catalog/parity/rpm/mod.rs uses. A `mod.rs`
# parent resolves children in its own directory, not in `mod/tests.rs`.
write_fixture "$fixture_root/crates/fixture/src/mod_parent/mod.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/mod_parent/tests.rs" 600

# (c) a declaration naming no scanned file is not a sibling.
write_fixture "$fixture_root/crates/fixture/src/unresolved_child.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF

"$checker" --root "$fixture_root" --allowlist "$allowlist" >/dev/null
sibling_report="$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report)"
grep -q $'path_sibling.rs\ttotal=4\tproduction=1\tinline_test=3\tsiblings=1\tsibling_tests=1200\treduction=1200' <<<"$sibling_report"
grep -q $'mod_parent/mod.rs\ttotal=3\tproduction=1\tinline_test=2\tsiblings=1\tsibling_tests=600\treduction=600' <<<"$sibling_report"
grep -q $'EXTRACTED: crates/fixture/src/path_sibling/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0' <<<"$sibling_report"
grep -q $'EXTRACTED: crates/fixture/src/mod_parent/tests.rs\ttotal=600\tproduction=600\tinline_test=0' <<<"$sibling_report"
if grep -q 'unresolved_child.rs.*siblings=' <<<"$sibling_report"; then
    echo "ERROR: an unresolved declaration produced sibling fields" >&2
    exit 1
fi
# The exempt-named sibling gains an EXTRACTED line but never an ordinary row.
if grep -q $'^crates/fixture/src/path_sibling/tests.rs\t' <<<"$sibling_report"; then
    echo "ERROR: exempt-named sibling gained an ordinary report row" >&2
    exit 1
fi
if grep -q $'^crates/fixture/src/mod_parent/tests.rs\t' <<<"$sibling_report"; then
    echo "ERROR: exempt-named sibling gained an ordinary report row" >&2
    exit 1
fi
# Issue #997: exempt-named files are reported and classified instead of being
# silently dropped. Classifying an exempt file must never move the exit code or
# the error set: this increment is report-only.
: > "$allowlist"
mkdir -p "$fixture_root/crates/fixture/src/gated_owner" "$fixture_root/crates/fixture/src/ungated_owner"
write_fixture "$fixture_root/crates/fixture/src/gated_owner.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/gated_owner/tests.rs" 1200
write_fixture "$fixture_root/crates/fixture/src/ungated_owner.rs" <<'EOF'
mod tests;
EOF
write_lines "$fixture_root/crates/fixture/src/ungated_owner/tests.rs" 1200
write_fixture "$fixture_root/crates/fixture/src/tests/inner_gate.rs" <<'EOF'
#![cfg(test)]
fn helper() {}
EOF

exempt_report="$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report)"
grep -q $'EXEMPT: crates/fixture/src/gated_owner/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0\tgate=test-gated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/ungated_owner/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0\tgate=ungated' <<<"$exempt_report"
grep -q $'EXEMPT: crates/fixture/src/tests/inner_gate.rs\ttotal=3\tproduction=0\tinline_test=3\tgate=test-gated' <<<"$exempt_report"
# The gated count depends on which sibling fixtures this suite has built, so
# assert the ungated count the labels above establish, not a total that other
# fixtures would shift.
grep -q 'EXEMPT SUMMARY: test-gated=[0-9]* ungated=3' <<<"$exempt_report"

# The same exempt file, declared with and without `#[cfg(test)]`, keeps the
# exit code and the ALLOWLISTED/ERROR set identical. An over-cap production file
# keeps the compared set non-empty, so the comparison is not vacuous.
write_lines "$fixture_root/crates/fixture/src/exempt_context_over_cap.rs" 1001
exempt_ungated_status=0
"$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/exempt-ungated.out" 2>&1 || exempt_ungated_status=$?
grep -E '^(ALLOWLISTED|ERROR):' "$fixture_root/exempt-ungated.out" | sort >"$fixture_root/exempt-ungated.errors" || true
write_fixture "$fixture_root/crates/fixture/src/ungated_owner.rs" <<'EOF'
#[cfg(test)]
mod tests;
EOF
exempt_gated_status=0
"$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/exempt-gated.out" 2>&1 || exempt_gated_status=$?
grep -E '^(ALLOWLISTED|ERROR):' "$fixture_root/exempt-gated.out" | sort >"$fixture_root/exempt-gated.errors" || true
if [[ "$exempt_ungated_status" != "1" || "$exempt_gated_status" != "1" ]]; then
    echo "ERROR: exempt-file fixtures did not exercise a real gate failure ($exempt_ungated_status, $exempt_gated_status)" >&2
    exit 1
fi
grep -q 'exempt_context_over_cap.rs has 1001 non-test lines' "$fixture_root/exempt-ungated.errors"
if ! diff -u "$fixture_root/exempt-ungated.errors" "$fixture_root/exempt-gated.errors"; then
    echo "ERROR: gating an exempt-named file changed the ALLOWLISTED/ERROR set" >&2
    exit 1
fi
grep -q $'EXEMPT: crates/fixture/src/ungated_owner/tests.rs\ttotal=1200\tproduction=1200\tinline_test=0\tgate=test-gated' \
    <<<"$("$checker" --root "$fixture_root" --allowlist "$allowlist" --report 2>/dev/null || true)"
rm "$fixture_root/crates/fixture/src/exempt_context_over_cap.rs"

# An over-cap exempt-named file may be allowlisted: its entry is recorded as
# used instead of being rejected as stale by the bookkeeping bug (#997).
echo 'crates/fixture/src/ungated_owner/tests.rs #123' > "$allowlist"
if ! "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/exempt-allowlist.out" 2>&1; then
    echo "ERROR: an allowlisted exempt-named file was rejected as stale" >&2
    cat "$fixture_root/exempt-allowlist.out" >&2
    exit 1
fi
if grep -q 'stale line-cap allowlist entry' "$fixture_root/exempt-allowlist.out"; then
    echo "ERROR: an allowlisted exempt-named file was reported stale" >&2
    exit 1
fi
: > "$allowlist"

write_fixture "$fixture_root/crates/fixture/src/malformed.rs" <<'EOF'
fn malformed( {
EOF
if "$checker" --root "$fixture_root" --allowlist "$allowlist" >"$fixture_root/malformed.out" 2>&1; then
    echo "ERROR: malformed Rust fixture unexpectedly passed" >&2
    exit 1
fi
grep -q 'failed to parse crates/fixture/src/malformed.rs' "$fixture_root/malformed.out"

echo "line-cap tests passed."
