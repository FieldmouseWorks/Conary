#!/usr/bin/env bash
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
scan_root="${1:-$repo_root}"

if [[ ! -d "$scan_root" ]]; then
  echo "ERROR: scan root does not exist: $scan_root" >&2
  exit 1
fi

cd "$scan_root"

find_action_files() {
  {
    find .github/workflows -maxdepth 1 -type f \( -name '*.yml' -o -name '*.yaml' \) -print 2>/dev/null || true
    find .github/actions -mindepth 2 -maxdepth 2 -type f -name action.yml -print 2>/dev/null || true
    find .github/actions -mindepth 2 -maxdepth 2 -type f -name action.yaml -print 2>/dev/null || true
  } | LC_ALL=C sort
}

extract_uses_refs() {
  local file="$1"
  awk -v file="$file" '
    /^[[:space:]]*-?[[:space:]]*uses:[[:space:]]*/ {
      ref = $0
      sub(/^[[:space:]]*-?[[:space:]]*uses:[[:space:]]*/, "", ref)
      sub(/[[:space:]]+#.*/, "", ref)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", ref)
      gsub(/^["'\'']|["'\'']$/, "", ref)
      if (ref != "") {
        printf "%s:%d:%s\n", file, NR, ref
      }
    }
  ' "$file"
}

is_local_ref() {
  [[ "$1" == ./* || "$1" == ../* ]]
}

is_pinned_external_ref() {
  [[ "$1" =~ @[0-9a-f]{40}$ ]]
}

# Positive policy rules match code, never commentary. Each rule reads the
# file through code_view, which blanks full-line comments and trailing
# ` #` comments, so a comment quoting a required command (for example a
# `# was: sudo timeout ...` note above an unbounded call) cannot satisfy its
# rule. Blanking keeps line numbers; a ` #` inside a quoted string is also
# blanked, which can only make a rule fail closed.
code_view() {
  sed -E -e 's/^[[:space:]]*#.*$//' -e 's/[[:space:]]+#.*$//' "$1"
}

# Process substitution, not a pipe: under pipefail an early `rg -q` exit
# could surface code_view's SIGPIPE as a false violation.
code_has_match() {
  rg -q --multiline -- "$2" < <(code_view "$1")
}

code_has_fixed() {
  rg -q --fixed-strings -- "$2" < <(code_view "$1")
}

mapfile -t action_files < <(find_action_files)
if [[ "${#action_files[@]}" -eq 0 ]]; then
  echo "ERROR: no GitHub workflow or action files found under $scan_root" >&2
  exit 1
fi

violations=()
while IFS= read -r entry; do
  file="${entry%%:*}"
  rest="${entry#*:}"
  line="${rest%%:*}"
  ref="${rest#*:}"

  if is_local_ref "$ref"; then
    continue
  fi
  if is_pinned_external_ref "$ref"; then
    continue
  fi

  violations+=("${file}:${line}: unpinned external action ${ref}")
done < <(
  for file in "${action_files[@]}"; do
    extract_uses_refs "$file"
  done
)

while IFS=: read -r file line _; do
  violations+=("${file}:${line}: composite-action description contains an unquoted mapping colon")
done < <(
  rg -n --no-heading -- \
    "^[[:space:]]*description:[[:space:]]+[^\"'|>].*:[[:space:]]" \
    .github/actions 2>/dev/null || true
)

shell_policy_action=".github/actions/setup-shell-policy-tools/action.yml"
if [[ ! -f "$shell_policy_action" ]]; then
  violations+=("${shell_policy_action}: missing shared shell-policy bootstrap")
else
  require_shell_policy_match() {
    local pattern="$1"
    local description="$2"

    if ! code_has_match "$shell_policy_action" "$pattern"; then
      violations+=("${shell_policy_action}: ${description}")
    fi
  }

  require_shell_policy_match \
    'if command -v rg >/dev/null; then[\s\S]*else[\s\S]*missing_packages\+=\(ripgrep\)' \
    'must reuse an existing rg before any apt operation'
  require_shell_policy_match \
    "python3 -I -c 'import yaml'[\s\S]*else[\s\S]*missing_packages\\+=\\(python3-yaml\\)" \
    'must reuse or provision PyYAML for structural workflow policy'
  require_shell_policy_match \
    'bash scripts/ci-install-ubuntu-packages\.sh "\$\{missing_packages\[@\]\}"' \
    'must delegate fallback policy-tool installation to the shared Ubuntu package owner'
fi

ubuntu_package_helper="scripts/ci-install-ubuntu-packages.sh"
if [[ ! -f "$ubuntu_package_helper" ]]; then
  violations+=("${ubuntu_package_helper}: missing shared hosted-Ubuntu package bootstrap")
else
  require_ubuntu_package_match() {
    local pattern="$1"
    local description="$2"

    if ! code_has_match "$ubuntu_package_helper" "$pattern"; then
      violations+=("${ubuntu_package_helper}: ${description}")
    fi
  }

  # These regexes intentionally match literal shell variables in the helper.
  # shellcheck disable=SC2016
  require_ubuntu_package_match \
    '\[\[ ! "\$package" =~ \^\[a-z0-9\]\[a-z0-9\+\.\-\]\*\$ \]\]' \
    'must reject untyped package arguments'
  require_ubuntu_package_match \
    'dpkg-query --show --showformat='\''\$\{Status\}'\''[\s\S]*missing_packages' \
    'must skip apt when every exact package is already installed'
  # shellcheck disable=SC2016
  require_ubuntu_package_match \
    'ubuntu_sources=/etc/apt/sources\.list\.d/ubuntu\.sources[\s\S]*! -f "\$ubuntu_sources" \|\| -L "\$ubuntu_sources"' \
    'must require the canonical Ubuntu source as a plain file'
  require_ubuntu_package_match \
    'Dir::Etc::sourcelist=\$\{ubuntu_sources\}[\s\S]*Dir::Etc::sourceparts=/dev/null' \
    'must isolate apt from image-provided third-party sources'
  require_ubuntu_package_match \
    'apt-get "\$\{apt_options\[@\]\}" update[\s\S]*apt-get "\$\{apt_options\[@\]\}" install -y --no-install-recommends' \
    'must use the isolated apt options for update and installation'
  require_ubuntu_package_match \
    'apt_options=\([^)]*"Acquire::http::Timeout=[1-9][0-9]*"[^)]*"Acquire::https::Timeout=[1-9][0-9]*"[^)]*"Acquire::Retries=[0-9]+"[^)]*\)' \
    'must pin apt fetch inactivity timeouts and retry count in the isolated apt options'
  # shellcheck disable=SC2016
  require_ubuntu_package_match \
    'sudo timeout --kill-after="\$\{apt_kill_grace_seconds\}s" "\$\{apt_update_timeout_seconds\}s"[\s\\]*apt-get "\$\{apt_options\[@\]\}" update' \
    'must run apt-get update under a root-owned outer timeout'
  # shellcheck disable=SC2016
  require_ubuntu_package_match \
    'sudo timeout --kill-after="\$\{apt_kill_grace_seconds\}s" "\$\{apt_install_timeout_seconds\}s"[\s\\]*env DEBIAN_FRONTEND=noninteractive[\s\\]*apt-get "\$\{apt_options\[@\]\}" install' \
    'must run apt-get install under a root-owned outer timeout'
  require_ubuntu_package_match \
    'apt-get "\$\{apt_options\[@\]\}" update --error-on=any' \
    'must fail apt-get update on any failed index fetch'

  # The bounds must fit the smallest consuming job budget
  # (ci-base-image-policy, timeout-minutes: 10) with two minutes left for
  # checkout and the job's own proof.
  ubuntu_package_apt_budget_seconds=480
  ubuntu_package_bound() {
    sed -n "s/^$1=\([1-9][0-9]*\)\$/\1/p" "$ubuntu_package_helper"
  }
  update_bound="$(ubuntu_package_bound apt_update_timeout_seconds)"
  install_bound="$(ubuntu_package_bound apt_install_timeout_seconds)"
  kill_grace="$(ubuntu_package_bound apt_kill_grace_seconds)"
  if [[ ! "$update_bound" =~ ^[0-9]+$ || ! "$install_bound" =~ ^[0-9]+$ || ! "$kill_grace" =~ ^[0-9]+$ ]]; then
    violations+=("${ubuntu_package_helper}: must define apt_update_timeout_seconds, apt_install_timeout_seconds, and apt_kill_grace_seconds as single positive integers")
  elif (( update_bound + install_bound + 2 * kill_grace > ubuntu_package_apt_budget_seconds )); then
    violations+=("${ubuntu_package_helper}: apt bounds total $(( update_bound + install_bound + 2 * kill_grace ))s, above the ${ubuntu_package_apt_budget_seconds}s budget of the smallest consuming job")
  fi
fi

ubuntu_package_callers=(
  ".github/actions/setup-rust-workspace/action.yml"
  ".github/actions/setup-shell-policy-tools/action.yml"
  ".github/actions/build-static-conary/action.yml"
  ".github/actions/test-generation-db-reflink/action.yml"
  ".github/workflows/release-build.yml"
)
for caller in "${ubuntu_package_callers[@]}"; do
  if [[ ! -f "$caller" ]]; then
    violations+=("${caller}: missing hosted-Ubuntu package bootstrap caller")
  elif ! code_has_fixed "$caller" 'bash scripts/ci-install-ubuntu-packages.sh'; then
    violations+=("${caller}: must use the shared hosted-Ubuntu package bootstrap")
  fi
done

compiler_cache_action=".github/actions/setup-rust-workspace/action.yml"
compiler_cache_summary_action=".github/actions/summarize-rust-cache/action.yml"
native_compiler_cache_action=".github/actions/setup-native-matrix-compiler-cache/action.yml"
if [[ ! -f "$compiler_cache_action" ]]; then
  violations+=("${compiler_cache_action}: missing protected compiler-cache owner")
else
  require_compiler_cache_match() {
    local pattern="$1"
    local description="$2"

    if ! code_has_match "$compiler_cache_action" "$pattern"; then
      violations+=("${compiler_cache_action}: ${description}")
    fi
  }

  require_compiler_cache_match \
    'compiler-cache:[\s\S]*default: "off"[\s\S]*COMPILER_CACHE_REQUEST: \$\{\{ inputs\.compiler-cache \}\}[\s\S]*off\|writer\|reader' \
    'must default off and reject unknown cache roles'
  # These policy regexes intentionally match literal shell variables.
  # shellcheck disable=SC2016
  require_compiler_cache_match \
    'namespace="protected-gnu-local-v1-\$\{identity\}"[\s\S]*exact_key="\$\{restore_prefix\}\$\{GITHUB_SHA\}"[\s\S]*CONARY_COMPILER_CACHE_NAMESPACE=\$namespace[\s\S]*SCCACHE_VERSION=0\.16\.0[\s\S]*SCCACHE_CACHE_SIZE=4G' \
    'must bind exact source, policy, implementation, and size to the local cache'
  require_compiler_cache_match \
    'actions/cache@[0-9a-f]{40}[\s\S]*key: \$\{\{ steps\.compiler-cache-policy\.outputs\.exact_key \}\}[\s\S]*restore-keys: \$\{\{ steps\.compiler-cache-policy\.outputs\.restore_prefix \}\}[\s\S]*actions/cache/restore@[0-9a-f]{40}[\s\S]*fail-on-cache-miss: true' \
    'must bulk-save one writable seed and fail closed on exact reader misses'
  # shellcheck disable=SC2016
  require_compiler_cache_match \
    'writer\) local_mode=READ_WRITE[\s\S]*reader\) local_mode=READ_ONLY[\s\S]*SCCACHE_LOCAL_RW_MODE=\$local_mode' \
    'must keep consumers read-only and the single primer writable'
  require_compiler_cache_match \
    'mozilla-actions/sccache-action@[0-9a-f]{40}[\s\S]*version: v0\.16\.0' \
    'must install the pinned sccache implementation and version'
  require_compiler_cache_match \
    'rustc=\%s[\s\S]*cargo=\%s[\s\S]*lock=\%s[\s\S]*target=\%s[\s\S]*cc=\%s[\s\S]*native_abi=\%s[\s\S]*rustflags=\%s[\s\S]*encoded_rustflags=\%s[\s\S]*incremental=\%s[\s\S]*dev_debug=\%s[\s\S]*test_debug=\%s' \
    'must bind toolchain, source dependency, native ABI, and codegen policy'
  # shellcheck disable=SC2016
  require_compiler_cache_match \
    'echo "RUSTC_WRAPPER=\$SCCACHE_PATH" >> "\$GITHUB_ENV"[\s\S]*"\$SCCACHE_PATH" --zero-stats' \
    'must activate the exact cache executable and reset per-job evidence'
fi

if [[ ! -f "$compiler_cache_summary_action" ]]; then
  violations+=("${compiler_cache_summary_action}: missing protected compiler-cache evidence owner")
else
  require_compiler_cache_summary_match() {
    local pattern="$1"
    local description="$2"

    if ! code_has_match "$compiler_cache_summary_action" "$pattern"; then
      violations+=("${compiler_cache_summary_action}: ${description}")
    fi
  }

  require_compiler_cache_summary_match \
    'policy:[\s\S]*default: gnu[\s\S]*gnu\) namespace_prefix=protected-gnu-local-v1[\s\S]*native-matrix\) namespace_prefix=native-matrix-musl-local-v1[\s\S]*CONARY_COMPILER_CACHE_NAMESPACE:-[\s\S]*namespace_prefix[\s\S]*\[0-9a-f\]\{64\}' \
    'must reject unknown policies and missing or non-exact protected namespaces'
  require_compiler_cache_summary_match \
    '--show-stats --stats-format json[\s\S]*\.version == "0\.16\.0"[\s\S]*startswith\("Local disk: "\)[\s\S]*\.stats\.compile_requests[\s\S]*\.stats\.cache_hits\.counts[\s\S]*\.stats\.cache_misses\.counts[\s\S]*\.stats\.cache_errors\.counts[\s\S]*\.stats\.cache_writes[\s\S]*\.stats\.cache_read_errors[\s\S]*\.stats\.cache_write_errors[\s\S]*\.stats\.cache_timeouts' \
      'must retain typed request, hit, miss, and error evidence from the pinned cache'
fi

if [[ ! -f "$native_compiler_cache_action" ]]; then
  violations+=("${native_compiler_cache_action}: missing native matrix compiler-cache owner")
else
  require_native_cache_action_fixed() {
    local needle="$1"
    local description="$2"

    if ! code_has_fixed "$native_compiler_cache_action" "$needle"; then
      violations+=("${native_compiler_cache_action}: ${description}")
    fi
  }

  for binding in \
    "rustc=%s" \
    "cargo=%s" \
    "lock=%s" \
    "target=x86_64-unknown-linux-musl" \
    "cc=%s" \
    "native_abi=%s" \
    "builder=%s" \
    "header_probe=%s" \
    "build_action=%s" \
    "cache_action=%s" \
    "features=default" \
    "test_harness=true" \
    "rustflags=%s" \
    "encoded_rustflags=%s" \
    "incremental=%s" \
    "dev_debug=%s" \
    "test_debug=%s"; do
    require_native_cache_action_fixed "$binding" \
      "native matrix compiler-cache identity must bind ${binding}"
  done
  require_native_cache_action_fixed \
    'echo "SCCACHE_CACHE_BACKEND=local-disk-bulk-v1"' \
    'native matrix compiler cache must use the local bulk backend'
  # This fixed string intentionally matches a literal shell variable.
  # shellcheck disable=SC2016
  require_native_cache_action_fixed \
    'echo "SCCACHE_DIR=$RUNNER_TEMP/native-matrix-sccache"' \
    'native matrix compiler cache must use its bounded runner-local directory'
  require_native_cache_action_fixed \
    'echo "SCCACHE_LOCAL_RW_MODE=READ_WRITE"' \
    'native matrix cache producer must write only to its runner-local seed'
  # shellcheck disable=SC2016
  require_native_cache_action_fixed \
    'namespace="native-matrix-musl-local-v1-${identity}"' \
    'native matrix cache must use its exact policy identity'
  # shellcheck disable=SC2016
  require_native_cache_action_fixed \
    'exact_key="${restore_prefix}${GITHUB_SHA}"' \
    'native matrix cache snapshots must bind the exact source commit'
  require_native_cache_action_fixed \
    'uses: actions/cache/restore@668228422ae6a00e4ad889ee87cd7109ec5666a7' \
    'native matrix cache restore must use the pinned split cache action'
  require_native_cache_action_fixed \
    'restore-keys: ${{ steps.policy.outputs.restore_prefix }}' \
    'native matrix cache must restore a compatible policy seed across source heads'
  require_native_cache_action_fixed \
    'uses: mozilla-actions/sccache-action@fc920bf0ec8de6ee65d409111f7ec508035751ba' \
    'native matrix cache must install the pinned sccache action'
  if rg -q --fixed-strings 'SCCACHE_GHA_ENABLED' "$native_compiler_cache_action"; then
    violations+=("${native_compiler_cache_action}: native matrix cache must not use the per-object GitHub backend")
  fi
fi

native_matrix_workflow=".github/workflows/pr-gate.yml"
if [[ -f "$native_matrix_workflow" ]]; then
  require_native_matrix_fixed() {
    local needle="$1"
    local description="$2"

    if ! code_has_fixed "$native_matrix_workflow" "$needle"; then
      violations+=("${native_matrix_workflow}: ${description}")
    fi
  }

  require_native_matrix_fixed \
    'uses: ./.github/actions/setup-native-matrix-compiler-cache' \
    'native matrix producer must consume the shared exact cache policy'
  require_native_matrix_fixed \
    'uses: actions/cache/save@668228422ae6a00e4ad889ee87cd7109ec5666a7' \
    'native matrix cache save must use the pinned split cache action'
  require_native_matrix_fixed \
    "if: \${{ steps.native-artifact-restore.outputs.cache-hit != 'true' && steps.native-cache.outputs.cache_hit != 'true' }}" \
    'native matrix cache must save only a new exact key'
  require_native_matrix_fixed \
    'key: ${{ steps.native-cache.outputs.exact_key }}' \
    'native matrix cache save must use the shared exact source key'
  require_native_matrix_fixed \
    'key: native-matrix-artifact-v1-${{ github.run_id }}-${{ github.sha }}' \
    'native matrix artifact reuse must bind the exact workflow run and source'
  require_native_matrix_fixed \
    "if: \${{ steps.native-artifact-restore.outputs.cache-hit == 'true' }}" \
    'native matrix artifact reuse must verify only a restored exact key'
  require_native_matrix_fixed \
    'Verify reusable exact-run matrix artifact' \
    'native matrix artifact reuse must retain an explicit verification boundary'
  require_native_matrix_fixed \
    'Save verified exact-run matrix artifact' \
    'native matrix artifact cache must be written only after fresh verification'
fi

trusted_seed_workflow=".github/workflows/merge-validation.yml"
if [[ -f "$trusted_seed_workflow" ]]; then
  require_trusted_seed_match() {
    local pattern="$1"
    local description="$2"

    if ! code_has_match "$trusted_seed_workflow" "$pattern"; then
      violations+=("${trusted_seed_workflow}: ${description}")
    fi
  }

  require_trusted_seed_match \
    'native-matrix-compiler-cache:[\s\S]*EXPECTED_REF: \$\{\{ github\.ref \}\}[\s\S]*refs/heads/main[\s\S]*setup-native-matrix-compiler-cache[\s\S]*build-static-conary[\s\S]*with-test-harness: "true"' \
    'trusted main must prime the complete native matrix compiler seed'
  require_trusted_seed_match \
    'policy: native-matrix[\s\S]*--stop-server[\s\S]*steps\.native-cache\.outputs\.cache_hit != '\''true'\''[\s\S]*actions/cache/save@[0-9a-f]{40}[\s\S]*key: \$\{\{ steps\.native-cache\.outputs\.exact_key \}\}' \
    'trusted main must retain typed evidence and save only a completed exact native seed'
fi

pr_cache_cleanup_workflow=".github/workflows/cleanup-pr-caches.yml"
if [[ ! -f "$pr_cache_cleanup_workflow" ]]; then
  violations+=("${pr_cache_cleanup_workflow}: missing closed-pull-request cache cleanup")
else
  require_pr_cache_cleanup_fixed() {
    local needle="$1"
    local description="$2"

    if ! code_has_fixed "$pr_cache_cleanup_workflow" "$needle"; then
      violations+=("${pr_cache_cleanup_workflow}: ${description}")
    fi
  }

  require_pr_cache_cleanup_fixed \
    'pull_request_target:' \
    'cache cleanup must use the base workflow for cross-repository pull requests'
  require_pr_cache_cleanup_fixed \
    'types: [closed]' \
    'cache cleanup must run only after a pull request closes'
  require_pr_cache_cleanup_fixed \
    'actions: write' \
    'cache cleanup must declare its exact mutation permission'
  require_pr_cache_cleanup_fixed \
    'cache_ref="refs/pull/${PR_NUMBER}/merge"' \
    'cache cleanup must derive only the closed pull request merge ref'
  require_pr_cache_cleanup_fixed \
    'gh api --paginate --method GET "repos/${GH_REPO}/actions/caches"' \
    'cache cleanup must enumerate every cache in the exact ref'
  require_pr_cache_cleanup_fixed \
    '-f ref="$cache_ref" -f per_page=100' \
    'cache cleanup enumeration must bind the exact merge ref'
  require_pr_cache_cleanup_fixed \
    'gh api --method DELETE "repos/${GH_REPO}/actions/caches/${cache_id}"' \
    'cache cleanup must delete only validated cache IDs from that ref'
  if rg -q --fixed-strings -- '--all' "$pr_cache_cleanup_workflow"; then
    violations+=("${pr_cache_cleanup_workflow}: cache cleanup must never use a repository-wide delete")
  fi
  if rg -q -- 'actions/checkout|pull_request\.head' "$pr_cache_cleanup_workflow"; then
    violations+=("${pr_cache_cleanup_workflow}: privileged cache cleanup must not consume pull request code")
  fi
fi

while IFS=: read -r file line _; do
  violations+=("${file}:${line}: unrestricted hosted-runner apt bootstrap")
done < <(
  rg -n --no-heading -- 'sudo[[:space:]]+(env[[:space:]]+[^[:space:]]+[[:space:]]+)?apt-get' \
    .github/actions .github/workflows 2>/dev/null || true
)

for workflow in .github/workflows/pr-gate.yml .github/workflows/merge-validation.yml; do
  [[ -f "$workflow" ]] || continue
  uses_count="$(rg -c --fixed-strings \
    'uses: ./.github/actions/setup-shell-policy-tools' "$workflow" || true)"
  uses_count="${uses_count:-0}"
  if [[ "$uses_count" -ne 3 ]]; then
    violations+=("${workflow}: expected 3 shared shell-policy bootstrap uses, found ${uses_count}")
  fi
  if rg -q -- 'Install shell policy tools|apt-get install -y ripgrep' "$workflow"; then
    violations+=("${workflow}: duplicated or unrestricted shell-policy apt bootstrap")
  fi
done

# Prerequisite reporting, decided from parsed YAML, not text. Scope: jobs in
# .github/workflows/*.y*ml only; composite actions and step-outcome checks
# (steps.<id>.outcome) are out of scope.
# 1. A step after the job's actions/checkout whose whole `run:` is a single
#    command comparing a `*_RESULT` env variable to success (`test "$X" =
#    success` or `[[ ... ]]`) must instead call
#    scripts/ci-require-prerequisite.sh, which is on disk after checkout.
#    Jobs without a checkout cannot reach the helper and keep the bare form.
# 2. Every `run: bash scripts/ci-require-prerequisite.sh JOB_ID "$VAR"` must
#    bind VAR (step env, else job env, else workflow env) to exactly
#    `${{ needs.<id>.result }}` with <id> equal to JOB_ID, so the annotation
#    names the job whose result it reports. Any other invocation form is a
#    violation.
while IFS= read -r violation; do
  [[ -n "$violation" ]] && violations+=("$violation")
done < <(
  python3 -I - <<'PY'
import glob
import re

import yaml

VALUE = r'"?\$\{?[A-Za-z0-9_]+_RESULT\}?"?\s+==?\s+"?success"?'
BARE_PREREQUISITE = re.compile(
    rf'^(?:test\s+{VALUE}|\[\[?\s+{VALUE}\s+\]\]?)$'
)
HELPER = "scripts/ci-require-prerequisite.sh"
INVOKES_HELPER = re.compile(
    r'(?:^|\s)(?:\./)?scripts/ci-require-prerequisite\.sh(?:\s|$)'
)
HELPER_CALL = re.compile(
    r'^bash\s+scripts/ci-require-prerequisite\.sh\s+([a-z0-9][a-z0-9-]*)\s+'
    r'"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?"$'
)
NEEDS_RESULT = re.compile(r'^\$\{\{\s*needs\.([A-Za-z0-9_-]+)\.result\s*\}\}$')


def env_of(node):
    env = node.get("env") if isinstance(node, dict) else None
    return env if isinstance(env, dict) else {}


def check_helper_call(path, workflow, job_id, job, step, run):
    where = f"{path}: job '{job_id}' step '{step.get('name', '<unnamed>')}'"
    call = HELPER_CALL.match(run)
    if not call:
        return (
            f"{where}: unrecognized {HELPER} invocation; use "
            f'bash {HELPER} JOB_ID "$VAR" with VAR bound to '
            "${{ needs.JOB_ID.result }}"
        )
    named_job, variable = call.groups()
    for scope in (env_of(step), env_of(job), env_of(workflow)):
        if variable in scope:
            binding = scope[variable]
            break
    else:
        return f"{where}: {HELPER} reads ${variable}, which no env binds"
    bound = NEEDS_RESULT.match(binding) if isinstance(binding, str) else None
    if not bound:
        return (
            f"{where}: {HELPER} reads ${variable}, bound to {binding!r} "
            "instead of ${{ needs.<id>.result }}"
        )
    if bound.group(1) != named_job:
        return (
            f"{where}: {HELPER} names job '{named_job}' but ${variable} "
            f"is ${{{{ needs.{bound.group(1)}.result }}}}"
        )
    return None


for path in sorted(glob.glob(".github/workflows/*.y*ml")):
    try:
        with open(path, encoding="utf-8") as handle:
            workflow = yaml.safe_load(handle)
    except (OSError, yaml.YAMLError) as error:
        print(f"{path}: cannot parse workflow for prerequisite policy: {error}")
        continue
    jobs = workflow.get("jobs") if isinstance(workflow, dict) else None
    if not isinstance(jobs, dict):
        continue
    for job_id, job in jobs.items():
        steps = job.get("steps") if isinstance(job, dict) else None
        if not isinstance(steps, list):
            continue
        checkout_index = None
        for index, step in enumerate(steps):
            uses = step.get("uses") if isinstance(step, dict) else None
            if isinstance(uses, str) and uses.split("@", 1)[0] == "actions/checkout":
                checkout_index = index
                break
        for index, step in enumerate(steps):
            run = step.get("run") if isinstance(step, dict) else None
            if not isinstance(run, str):
                continue
            run = run.strip()
            if INVOKES_HELPER.search(run):
                violation = check_helper_call(path, workflow, job_id, job, step, run)
                if violation:
                    print(violation)
            elif (
                checkout_index is not None
                and index > checkout_index
                and BARE_PREREQUISITE.match(run)
            ):
                print(
                    f"{path}: job '{job_id}' step '{step.get('name', '<unnamed>')}': "
                    "bare prerequisite comparison after checkout; use "
                    f"bash {HELPER} JOB_ID RESULT"
                )
PY
)

if [[ "${#violations[@]}" -ne 0 ]]; then
  printf 'ERROR: GitHub Actions policy violations found:\n' >&2
  printf '  %s\n' "${violations[@]}" >&2
  exit 1
fi

echo "GitHub Actions runtime pins are fully pinned."
