#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

# Replace exactly one occurrence of OLD with NEW in FILE, so a fixture edit
# that no longer matches its source fails here instead of testing nothing.
replace_once() {
  python3 -I - "$1" "$2" "$3" <<'PY'
import sys

path, old, new = sys.argv[1:4]
with open(path, encoding="utf-8") as handle:
    text = handle.read()
count = text.count(old)
if count != 1:
    sys.exit(f"{path}: fixture edit expected one match, found {count}: {old!r}")
with open(path, "w", encoding="utf-8") as handle:
    handle.write(text.replace(old, new))
PY
}

write_fixture() {
  local root="$1"
  local uses_ref="$2"

  mkdir -p \
    "$root/.github/workflows" \
    "$root/.github/actions/setup-rust-workspace" \
    "$root/.github/actions/setup-native-matrix-compiler-cache" \
    "$root/.github/actions/summarize-rust-cache" \
    "$root/.github/actions/setup-shell-policy-tools" \
    "$root/.github/actions/build-static-conary" \
    "$root/.github/actions/test-generation-db-reflink" \
    "$root/scripts"
  cp scripts/ci-install-ubuntu-packages.sh "$root/scripts/"
  cp .github/actions/setup-rust-workspace/action.yml \
    "$root/.github/actions/setup-rust-workspace/action.yml"
  cp .github/actions/setup-native-matrix-compiler-cache/action.yml \
    "$root/.github/actions/setup-native-matrix-compiler-cache/action.yml"
  cp .github/actions/summarize-rust-cache/action.yml \
    "$root/.github/actions/summarize-rust-cache/action.yml"
  cp .github/workflows/cleanup-pr-caches.yml \
    "$root/.github/workflows/cleanup-pr-caches.yml"
  # The apt budget is derived from this workflow's ci-base-image-policy job.
  cp .github/workflows/pr-gate.yml "$root/.github/workflows/pr-gate.yml"
  cat > "$root/.github/workflows/policy.yml" <<EOF
name: policy
on: workflow_dispatch
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: ${uses_ref}
      - uses: ./.github/actions/setup-shell-policy-tools
      - uses: ./.github/actions/setup-rust-workspace
      - uses: actions/cache@668228422ae6a00e4ad889ee87cd7109ec5666a7
EOF

  cat > "$root/.github/workflows/prerequisite.yml" <<EOF
name: prerequisite
on: workflow_dispatch
jobs:
  primer:
    runs-on: ubuntu-latest
    steps:
      - run: "true"
  with-checkout:
    runs-on: ubuntu-latest
    needs: primer
    steps:
      - uses: ${uses_ref}
      - name: Require exact compiler-cache seed
        env:
          PRIMER_RESULT: \${{ needs.primer.result }}
        run: bash scripts/ci-require-prerequisite.sh primer "\$PRIMER_RESULT"
  without-checkout:
    runs-on: ubuntu-latest
    env:
      PRIMER_RESULT: success
    steps:
      - name: Require exact compiler-cache seed
        run: test "\$PRIMER_RESULT" = success
EOF

  cat > "$root/.github/workflows/release-build.yml" <<'EOF'
name: release-build
on: workflow_dispatch
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: bash scripts/ci-install-ubuntu-packages.sh libssl-dev
EOF

  cat > "$root/.github/actions/setup-shell-policy-tools/action.yml" <<'EOF'
name: setup-shell-policy-tools
runs:
  using: composite
  steps:
    - shell: bash
      run: |
        missing_packages=()
        if command -v rg >/dev/null; then
          rg --version
        else
          missing_packages+=(ripgrep)
        fi
        if python3 -I -c 'import yaml' >/dev/null 2>&1; then
          python3 -I -c 'import yaml; print(yaml.__version__)'
        else
          missing_packages+=(python3-yaml)
        fi
        if [[ "${#missing_packages[@]}" -gt 0 ]]; then
          bash scripts/ci-install-ubuntu-packages.sh "${missing_packages[@]}"
        fi
EOF

  cat > "$root/.github/actions/build-static-conary/action.yml" <<'EOF'
name: build-static-conary
runs:
  using: composite
  steps:
    - run: bash scripts/ci-install-ubuntu-packages.sh musl-tools
      shell: bash
EOF

  cat > "$root/.github/actions/test-generation-db-reflink/action.yml" <<'EOF'
name: test-generation-db-reflink
runs:
  using: composite
  steps:
    - run: bash scripts/ci-install-ubuntu-packages.sh btrfs-progs
      shell: bash
EOF
}

bad_root="$tmpdir/bad"
good_root="$tmpdir/good"
unsafe_shell_root="$tmpdir/unsafe-shell"
unsafe_python_yaml_root="$tmpdir/unsafe-python-yaml"
unsafe_apt_root="$tmpdir/unsafe-apt"
unsafe_source_root="$tmpdir/unsafe-source"
unsafe_prereq_root="$tmpdir/unsafe-prerequisite"
unsafe_prereq_id_root="$tmpdir/unsafe-prerequisite-id"
unsafe_apt_comment_root="$tmpdir/unsafe-apt-comment"
unsafe_apt_continuation_root="$tmpdir/unsafe-apt-continuation"
unsafe_prereq_path_root="$tmpdir/unsafe-prerequisite-path"
unsafe_workflow_read_root="$tmpdir/unsafe-workflow-read"
unsafe_budget_job_root="$tmpdir/unsafe-budget-job"
unsafe_budget_type_root="$tmpdir/unsafe-budget-type"
unsafe_apt_semicolon_root="$tmpdir/unsafe-apt-semicolon"
unsafe_prereq_order_root="$tmpdir/unsafe-prerequisite-order"
unsafe_apt_bound_root="$tmpdir/unsafe-apt-bound"
unsafe_apt_budget_root="$tmpdir/unsafe-apt-budget"
unsafe_cache_root="$tmpdir/unsafe-cache"
unsafe_native_cache_root="$tmpdir/unsafe-native-cache"
unsafe_pr_cleanup_root="$tmpdir/unsafe-pr-cleanup"
unsafe_action_yaml_root="$tmpdir/unsafe-action-yaml"
write_fixture "$bad_root" "actions/checkout@v6"
write_fixture "$good_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_shell_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_python_yaml_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_apt_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_source_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_prereq_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_prereq_id_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_apt_comment_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
for root in "$unsafe_apt_continuation_root" "$unsafe_prereq_path_root" \
  "$unsafe_workflow_read_root" "$unsafe_budget_job_root" "$unsafe_budget_type_root" \
  "$unsafe_apt_semicolon_root" "$unsafe_prereq_order_root"; do
  write_fixture "$root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
done
write_fixture "$unsafe_apt_bound_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_apt_budget_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_cache_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_native_cache_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_pr_cleanup_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
write_fixture "$unsafe_action_yaml_root" "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
sed -i 's/if command -v rg >\/dev\/null; then/if false; then/' \
  "$unsafe_shell_root/.github/actions/setup-shell-policy-tools/action.yml"
sed -i 's/missing_packages+=(python3-yaml)/missing_packages+=(ripgrep)/' \
  "$unsafe_python_yaml_root/.github/actions/setup-shell-policy-tools/action.yml"
sed -i \
  's#bash scripts/ci-install-ubuntu-packages.sh libseccomp-dev#sudo apt-get update#' \
  "$unsafe_apt_root/.github/actions/setup-rust-workspace/action.yml"
sed -i \
  's#/etc/apt/sources.list.d/ubuntu.sources#/etc/apt/sources.list#' \
  "$unsafe_source_root/scripts/ci-install-ubuntu-packages.sh"
# The fixture edit intentionally matches literal shell variables.
# shellcheck disable=SC2016
sed -i \
  's#^sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" \\$#sudo \\#' \
  "$unsafe_apt_bound_root/scripts/ci-install-ubuntu-packages.sh"
# Revert the checkout job's step to the bare comparison.
# shellcheck disable=SC2016
sed -i \
  's#bash scripts/ci-require-prerequisite.sh primer "$PRIMER_RESULT"#test "$PRIMER_RESULT" = success#' \
  "$unsafe_prereq_root/.github/workflows/prerequisite.yml"
# The helper names a job other than the one whose result its env binds.
# shellcheck disable=SC2016
replace_once "$unsafe_prereq_id_root/.github/workflows/prerequisite.yml" \
  'bash scripts/ci-require-prerequisite.sh primer "$PRIMER_RESULT"' \
  'bash scripts/ci-require-prerequisite.sh other-job "$PRIMER_RESULT"'
# A comment quoting the bounded update must not satisfy the rules that the
# unbounded call below it breaks.
# shellcheck disable=SC2016
replace_once "$unsafe_apt_comment_root/scripts/ci-install-ubuntu-packages.sh" \
  'sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" \
  apt-get "${apt_options[@]}" update --error-on=any || update_status=$?' \
  '# was: sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" apt-get "${apt_options[@]}" update --error-on=any
sudo apt-get "${apt_options[@]}" update || update_status=$?'
# A comment line inside the `\` continuation ends the bounded command, so
# apt-get on the next line runs unbounded.
# shellcheck disable=SC2016
replace_once "$unsafe_apt_continuation_root/scripts/ci-install-ubuntu-packages.sh" \
  '"${apt_update_timeout_seconds}s" \
  apt-get "${apt_options[@]}" update --error-on=any' \
  '"${apt_update_timeout_seconds}s" \
  # bounded update
  apt-get "${apt_options[@]}" update --error-on=any'
# The helper reached through another path must still be the exact form.
# shellcheck disable=SC2016
replace_once "$unsafe_prereq_path_root/.github/workflows/prerequisite.yml" \
  'bash scripts/ci-require-prerequisite.sh primer "$PRIMER_RESULT"' \
  'bash "$GITHUB_WORKSPACE/scripts/ci-require-prerequisite.sh" primer "$PRIMER_RESULT"'
# Unparseable and non-UTF-8 workflows sort before prerequisite.yml, whose
# bare comparison must still be reported.
printf 'name: broken\njobs: [unclosed\n' \
  > "$unsafe_workflow_read_root/.github/workflows/broken.yml"
printf 'name: bad-\377-utf8\non: workflow_dispatch\n' \
  > "$unsafe_workflow_read_root/.github/workflows/bad-utf8.yml"
# shellcheck disable=SC2016
replace_once "$unsafe_workflow_read_root/.github/workflows/prerequisite.yml" \
  'bash scripts/ci-require-prerequisite.sh primer "$PRIMER_RESULT"' \
  'test "$PRIMER_RESULT" = success'
budget_job_header='  ci-base-image-policy:
    name: ci-base-image-policy
    runs-on: ubuntu-latest
    timeout-minutes: 10
'
replace_once "$unsafe_budget_job_root/.github/workflows/pr-gate.yml" \
  "$budget_job_header" "${budget_job_header%10?}7
"
replace_once "$unsafe_budget_type_root/.github/workflows/pr-gate.yml" \
  "$budget_job_header" "${budget_job_header%10?}ten
"
# A `;#` comment after an unbounded update quotes the bounded one.
# shellcheck disable=SC2016
replace_once "$unsafe_apt_semicolon_root/scripts/ci-install-ubuntu-packages.sh" \
  'sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" \
  apt-get "${apt_options[@]}" update --error-on=any || update_status=$?' \
  'sudo apt-get "${apt_options[@]}" update --error-on=any || update_status=$?;#sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" apt-get "${apt_options[@]}" update'
# Correctly bound helper calls where the helper is not on disk: before the
# checkout, and in a job without one.
cat > "$unsafe_prereq_order_root/.github/workflows/prerequisite-order.yml" <<EOF
name: prerequisite-order
on: workflow_dispatch
jobs:
  primer:
    runs-on: ubuntu-latest
    steps:
      - run: "true"
  helper-before-checkout:
    runs-on: ubuntu-latest
    needs: primer
    steps:
      - name: Require primer before checkout
        env:
          PRIMER_RESULT: \${{ needs.primer.result }}
        run: bash scripts/ci-require-prerequisite.sh primer "\$PRIMER_RESULT"
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
  helper-without-checkout:
    runs-on: ubuntu-latest
    needs: primer
    steps:
      - name: Require primer without checkout
        env:
          PRIMER_RESULT: \${{ needs.primer.result }}
        run: bash scripts/ci-require-prerequisite.sh primer "\$PRIMER_RESULT"
EOF
# find that lists everything but reports an error, as for an unreadable
# action directory.
find_shim="$tmpdir/find-shim"
mkdir -p "$find_shim"
printf '#!/bin/sh\n"%s" "$@"\nexit 1\n' "$(command -v find)" > "$find_shim/find"
chmod +x "$find_shim/find"
# find that fails only for one listed directory, so each listing's own status
# check is proven separately.
find_workflows_shim="$tmpdir/find-workflows-shim"
find_actions_shim="$tmpdir/find-actions-shim"
mkdir -p "$find_workflows_shim" "$find_actions_shim"
# The shim bodies intentionally keep $1 and $@ literal for the generated script.
# shellcheck disable=SC2016
printf '#!/bin/sh\n"%s" "$@" || exit\n[ "$1" = .github/workflows ] && exit 1\nexit 0\n' "$(command -v find)" > "$find_workflows_shim/find"
# shellcheck disable=SC2016
printf '#!/bin/sh\n"%s" "$@" || exit\n[ "$1" = .github/actions ] && exit 1\nexit 0\n' "$(command -v find)" > "$find_actions_shim/find"
chmod +x "$find_workflows_shim/find" "$find_actions_shim/find"
# python3 that cannot run the structural scans.
python_shim="$tmpdir/python-shim"
mkdir -p "$python_shim"
printf '#!/bin/sh\nexit 1\n' > "$python_shim/python3"
chmod +x "$python_shim/python3"
# rg that fails every scan with an error status.
rg_shim="$tmpdir/rg-shim"
mkdir -p "$rg_shim"
printf '#!/bin/sh\nexit 2\n' > "$rg_shim/rg"
chmod +x "$rg_shim/rg"
sed -i 's/^apt_install_timeout_seconds=[0-9]*$/apt_install_timeout_seconds=3600/' \
  "$unsafe_apt_budget_root/scripts/ci-install-ubuntu-packages.sh"
sed -i 's/version: v0\.16\.0/version: latest/' \
  "$unsafe_cache_root/.github/actions/setup-rust-workspace/action.yml"
sed -i \
  's#restore-keys: ${{ steps.policy.outputs.restore_prefix }}#restore-keys: native-matrix-musl-local-v1-#' \
  "$unsafe_native_cache_root/.github/actions/setup-native-matrix-compiler-cache/action.yml"
sed -i \
  's#cache_ref="refs/pull/${PR_NUMBER}/merge"#cache_ref="refs/heads/main"#' \
  "$unsafe_pr_cleanup_root/.github/workflows/cleanup-pr-caches.yml"
sed -i \
  's/description: "Exact compiler-cache role: off, writer, or reader\."/description: Exact compiler-cache role: off, writer, or reader./' \
  "$unsafe_action_yaml_root/.github/actions/setup-rust-workspace/action.yml"

if bash scripts/check-github-action-runtimes.sh "$bad_root" >"$tmpdir/bad.out" 2>"$tmpdir/bad.err"; then
  echo "expected unpinned action fixture to fail" >&2
  cat "$tmpdir/bad.out" >&2
  cat "$tmpdir/bad.err" >&2
  exit 1
fi

if ! rg -q 'actions/checkout@v6' "$tmpdir/bad.err"; then
  echo "expected failure to name the unpinned action" >&2
  cat "$tmpdir/bad.err" >&2
  exit 1
fi

bash scripts/check-github-action-runtimes.sh "$good_root"

if bash scripts/check-github-action-runtimes.sh "$unsafe_shell_root" \
  >"$tmpdir/unsafe-shell.out" 2>"$tmpdir/unsafe-shell.err"; then
  echo "expected unconditional shell-policy apt fixture to fail" >&2
  cat "$tmpdir/unsafe-shell.out" >&2
  cat "$tmpdir/unsafe-shell.err" >&2
  exit 1
fi

if ! rg -q 'must reuse an existing rg before any apt operation' \
  "$tmpdir/unsafe-shell.err"; then
  echo "expected failure to name the missing existing-rg guard" >&2
  cat "$tmpdir/unsafe-shell.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_python_yaml_root" \
  >"$tmpdir/unsafe-python-yaml.out" 2>"$tmpdir/unsafe-python-yaml.err"; then
  echo "expected missing PyYAML bootstrap fixture to fail" >&2
  cat "$tmpdir/unsafe-python-yaml.out" >&2
  cat "$tmpdir/unsafe-python-yaml.err" >&2
  exit 1
fi

if ! rg -q 'must reuse or provision PyYAML for structural workflow policy' \
  "$tmpdir/unsafe-python-yaml.err"; then
  echo "expected failure to name the missing PyYAML bootstrap" >&2
  cat "$tmpdir/unsafe-python-yaml.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_apt_root" \
  >"$tmpdir/unsafe-apt.out" 2>"$tmpdir/unsafe-apt.err"; then
  echo "expected unrestricted hosted-runner apt fixture to fail" >&2
  cat "$tmpdir/unsafe-apt.out" >&2
  cat "$tmpdir/unsafe-apt.err" >&2
  exit 1
fi

if ! rg -q 'unrestricted hosted-runner apt bootstrap' \
  "$tmpdir/unsafe-apt.err"; then
  echo "expected failure to name the unrestricted hosted-runner apt" >&2
  cat "$tmpdir/unsafe-apt.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_source_root" \
  >"$tmpdir/unsafe-source.out" 2>"$tmpdir/unsafe-source.err"; then
  echo "expected noncanonical Ubuntu apt source fixture to fail" >&2
  cat "$tmpdir/unsafe-source.out" >&2
  cat "$tmpdir/unsafe-source.err" >&2
  exit 1
fi

if ! rg -q 'must require the canonical Ubuntu source as a plain file' \
  "$tmpdir/unsafe-source.err"; then
  echo "expected failure to name the noncanonical Ubuntu apt source" >&2
  cat "$tmpdir/unsafe-source.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_prereq_root" \
  >"$tmpdir/unsafe-prerequisite.out" 2>"$tmpdir/unsafe-prerequisite.err"; then
  echo "expected bare prerequisite comparison fixture to fail" >&2
  cat "$tmpdir/unsafe-prerequisite.out" >&2
  cat "$tmpdir/unsafe-prerequisite.err" >&2
  exit 1
fi

if ! rg -q "prerequisite.yml: job 'with-checkout' step 'Require exact compiler-cache seed': bare prerequisite comparison" \
  "$tmpdir/unsafe-prerequisite.err"; then
  echo "expected failure to name the workflow, job, and step with the bare prerequisite comparison" >&2
  cat "$tmpdir/unsafe-prerequisite.err" >&2
  exit 1
fi

if rg -q "job 'without-checkout'" "$tmpdir/unsafe-prerequisite.err"; then
  echo "a job without a checkout must keep the bare prerequisite comparison" >&2
  cat "$tmpdir/unsafe-prerequisite.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_prereq_id_root" \
  >"$tmpdir/unsafe-prerequisite-id.out" 2>"$tmpdir/unsafe-prerequisite-id.err"; then
  echo "expected mismatched prerequisite job id fixture to fail" >&2
  cat "$tmpdir/unsafe-prerequisite-id.out" >&2
  cat "$tmpdir/unsafe-prerequisite-id.err" >&2
  exit 1
fi

if ! rg -q --fixed-strings "prerequisite.yml: job 'with-checkout' step 'Require exact compiler-cache seed': scripts/ci-require-prerequisite.sh names job 'other-job' but \$PRIMER_RESULT is \${{ needs.primer.result }}" \
  "$tmpdir/unsafe-prerequisite-id.err"; then
  echo "expected failure to name the workflow, job, and step with the mismatched prerequisite job id" >&2
  cat "$tmpdir/unsafe-prerequisite-id.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_apt_comment_root" \
  >"$tmpdir/unsafe-apt-comment.out" 2>"$tmpdir/unsafe-apt-comment.err"; then
  echo "expected commented-out bounded apt-get update fixture to fail" >&2
  cat "$tmpdir/unsafe-apt-comment.out" >&2
  cat "$tmpdir/unsafe-apt-comment.err" >&2
  exit 1
fi

for rule in \
  'must run apt-get update under a root-owned outer timeout' \
  'must fail apt-get update on any failed index fetch'; do
  if ! rg -q --fixed-strings -- "$rule" "$tmpdir/unsafe-apt-comment.err"; then
    echo "expected a comment not to satisfy the rule: $rule" >&2
    cat "$tmpdir/unsafe-apt-comment.err" >&2
    exit 1
  fi
done

if bash scripts/check-github-action-runtimes.sh "$unsafe_apt_bound_root" \
  >"$tmpdir/unsafe-apt-bound.out" 2>"$tmpdir/unsafe-apt-bound.err"; then
  echo "expected unbounded apt-get update fixture to fail" >&2
  cat "$tmpdir/unsafe-apt-bound.out" >&2
  cat "$tmpdir/unsafe-apt-bound.err" >&2
  exit 1
fi

if ! rg -q 'must run apt-get update under a root-owned outer timeout' \
  "$tmpdir/unsafe-apt-bound.err"; then
  echo "expected failure to name the missing outer apt-get update timeout" >&2
  cat "$tmpdir/unsafe-apt-bound.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_apt_budget_root" \
  >"$tmpdir/unsafe-apt-budget.out" 2>"$tmpdir/unsafe-apt-budget.err"; then
  echo "expected over-budget apt bound fixture to fail" >&2
  cat "$tmpdir/unsafe-apt-budget.out" >&2
  cat "$tmpdir/unsafe-apt-budget.err" >&2
  exit 1
fi

if ! rg -q 'apt bounds total 3770s, above the 480s budget of the smallest consuming job' \
  "$tmpdir/unsafe-apt-budget.err"; then
  echo "expected failure to name the over-budget apt bounds" >&2
  cat "$tmpdir/unsafe-apt-budget.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_cache_root" \
  >"$tmpdir/unsafe-cache.out" 2>"$tmpdir/unsafe-cache.err"; then
  echo "expected unpinned compiler-cache fixture to fail" >&2
  cat "$tmpdir/unsafe-cache.out" >&2
  cat "$tmpdir/unsafe-cache.err" >&2
  exit 1
fi

if ! rg -q 'must install the pinned sccache implementation and version' \
  "$tmpdir/unsafe-cache.err"; then
  echo "expected failure to name the unpinned compiler cache" >&2
  cat "$tmpdir/unsafe-cache.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_native_cache_root" \
  >"$tmpdir/unsafe-native-cache.out" 2>"$tmpdir/unsafe-native-cache.err"; then
  echo "expected unbound native compiler-cache fixture to fail" >&2
  cat "$tmpdir/unsafe-native-cache.out" >&2
  cat "$tmpdir/unsafe-native-cache.err" >&2
  exit 1
fi

if ! rg -q 'must restore a compatible policy seed across source heads' \
  "$tmpdir/unsafe-native-cache.err"; then
  echo "expected failure to name the unbound native compiler-cache restore" >&2
  cat "$tmpdir/unsafe-native-cache.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_pr_cleanup_root" \
  >"$tmpdir/unsafe-pr-cleanup.out" 2>"$tmpdir/unsafe-pr-cleanup.err"; then
  echo "expected broad pull-request cache cleanup fixture to fail" >&2
  cat "$tmpdir/unsafe-pr-cleanup.out" >&2
  cat "$tmpdir/unsafe-pr-cleanup.err" >&2
  exit 1
fi

if ! rg -q 'must derive only the closed pull request merge ref' \
  "$tmpdir/unsafe-pr-cleanup.err"; then
  echo "expected failure to name the broadened cache cleanup ref" >&2
  cat "$tmpdir/unsafe-pr-cleanup.err" >&2
  exit 1
fi

if bash scripts/check-github-action-runtimes.sh "$unsafe_action_yaml_root" \
  >"$tmpdir/unsafe-action-yaml.out" 2>"$tmpdir/unsafe-action-yaml.err"; then
  echo "expected unquoted composite-action mapping fixture to fail" >&2
  cat "$tmpdir/unsafe-action-yaml.out" >&2
  cat "$tmpdir/unsafe-action-yaml.err" >&2
  exit 1
fi

if ! rg -q 'composite-action description contains an unquoted mapping colon' \
  "$tmpdir/unsafe-action-yaml.err"; then
  echo "expected failure to name the invalid action-manifest description" >&2
  cat "$tmpdir/unsafe-action-yaml.err" >&2
  exit 1
fi

# expect_violations LABEL ROOT MESSAGE...: the checker must fail on ROOT and
# report every MESSAGE (fixed strings). PATH_PREFIX, when set, is prepended
# to PATH for that checker run only.
expect_violations() {
  local label="$1"
  local root="$2"
  local message
  shift 2

  if PATH="${PATH_PREFIX:+${PATH_PREFIX}:}$PATH" \
    bash scripts/check-github-action-runtimes.sh "$root" \
    >"$tmpdir/${label}.out" 2>"$tmpdir/${label}.err"; then
    echo "expected ${label} fixture to fail" >&2
    cat "$tmpdir/${label}.out" "$tmpdir/${label}.err" >&2
    exit 1
  fi
  for message in "$@"; do
    if ! rg -q --fixed-strings -- "$message" "$tmpdir/${label}.err"; then
      echo "expected ${label} failure to report: ${message}" >&2
      cat "$tmpdir/${label}.err" >&2
      exit 1
    fi
  done
}

expect_violations apt-continuation "$unsafe_apt_continuation_root" \
  'scripts/ci-install-ubuntu-packages.sh: must run apt-get update under a root-owned outer timeout'
expect_violations prerequisite-path "$unsafe_prereq_path_root" \
  "prerequisite.yml: job 'with-checkout' step 'Require exact compiler-cache seed': unrecognized scripts/ci-require-prerequisite.sh invocation"
expect_violations workflow-read "$unsafe_workflow_read_root" \
  '.github/workflows/bad-utf8.yml: cannot read or parse workflow for prerequisite policy: UnicodeDecodeError' \
  '.github/workflows/broken.yml: cannot read or parse workflow for prerequisite policy:' \
  "prerequisite.yml: job 'with-checkout' step 'Require exact compiler-cache seed': bare prerequisite comparison"
PATH_PREFIX="$python_shim" expect_violations python-unavailable "$good_root" \
  'prerequisite structural policy scan did not complete (python3 exit 1)' \
  '.github/workflows/pr-gate.yml: cannot derive the apt budget from job ci-base-image-policy: python3 exited 1'
expect_violations apt-semicolon "$unsafe_apt_semicolon_root" \
  'scripts/ci-install-ubuntu-packages.sh: must run apt-get update under a root-owned outer timeout'
expect_violations prerequisite-order "$unsafe_prereq_order_root" \
  "prerequisite-order.yml: job 'helper-before-checkout' step 'Require primer before checkout': scripts/ci-require-prerequisite.sh runs before the job's actions/checkout" \
  "prerequisite-order.yml: job 'helper-without-checkout' step 'Require primer without checkout': scripts/ci-require-prerequisite.sh runs in a job without actions/checkout"
PATH_PREFIX="$find_shim" expect_violations find-error "$good_root" \
  'cannot list every workflow and action file (find failed); the action pin scan is incomplete'
PATH_PREFIX="$find_workflows_shim" expect_violations find-workflows-error "$good_root" \
  'cannot list every workflow and action file (find failed); the action pin scan is incomplete'
PATH_PREFIX="$find_actions_shim" expect_violations find-actions-error "$good_root" \
  'cannot list every workflow and action file (find failed); the action pin scan is incomplete'
PATH_PREFIX="$rg_shim" expect_violations rg-unavailable "$good_root" \
  'policy scan failed: rg -q --fixed-strings SCCACHE_GHA_ENABLED' \
  'policy scan failed: composite-action description scan of .github/actions' \
  'policy scan failed: hosted-runner apt scan of .github/actions and .github/workflows' \
  'scripts/ci-install-ubuntu-packages.sh: must reject untyped package arguments'
expect_violations budget-job "$unsafe_budget_job_root" \
  'apt bounds total 410s, above the 300s budget of the smallest consuming job (ci-base-image-policy timeout-minutes less 120s)'
expect_violations budget-type "$unsafe_budget_type_root" \
  "cannot derive the apt budget from job ci-base-image-policy: error jobs.ci-base-image-policy.timeout-minutes is 'ten', not a positive integer"

echo "GitHub Actions runtime policy fixtures passed."

# Exercise pinned archive verification and the runner tool-cache handoff.
python3 -I scripts/test-sccache-archive.py
