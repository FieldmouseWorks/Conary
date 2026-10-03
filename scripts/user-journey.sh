#!/usr/bin/env bash
# scripts/user-journey.sh
#
# Run the real Conary user journey inside a stock distribution container:
# initialise the system, adopt it, then install, execute, and remove one
# digest-pinned upstream package per source profile. Every stage is recorded
# even when an earlier stage fails, and the result is written as JSON evidence.
#
# Harness-only environment knobs:
#   JOURNEY_CONARY   command to run (default: conary)
#   JOURNEY_BIN_ROOT directory prefix prepended to each row's binary path
#                    (default: empty, so the real /usr/bin/tree is checked)
set -euo pipefail

readonly REASON_OK="ok"
readonly REASON_COMMAND_FAILED="command_failed"
readonly REASON_PREEXISTING_BINARY="preexisting_binary"
readonly REASON_BINARY_MISSING="binary_missing"
readonly REASON_BINARY_DIGEST_MISMATCH="binary_digest_mismatch"
readonly REASON_EXECUTION_FAILED="execution_failed"
readonly REASON_STILL_PRESENT="still_present"

usage() {
  echo "usage: $0 --host <profile> --packages <tsv> --downloads <dir> --evidence <json-out>" >&2
  exit 2
}

harness_error() {
  echo "user-journey harness error: $*" >&2
  exit 2
}

# Escape one string for a JSON double-quoted value: backslash, double quote,
# and every control character (named escapes where JSON defines them).
json_escape() {
  local s="$1" out="" c code esc i
  for (( i=0; i<${#s}; i++ )); do
    c="${s:i:1}"
    case "$c" in
      "\\") out+="\\\\" ;;
      '"') out+='\"' ;;
      $'\n') out+='\n' ;;
      $'\r') out+='\r' ;;
      $'\t') out+='\t' ;;
      $'\b') out+='\b' ;;
      $'\f') out+='\f' ;;
      [[:cntrl:]])
        printf -v code '%d' "'$c"
        printf -v esc '\\u%04x' "$code"
        out+="$esc"
        ;;
      *) out+="$c" ;;
    esac
  done
  printf '%s' "$out"
}

sha256_file() {
  local digest
  digest="$(sha256sum "$1")"
  printf '%s' "${digest%% *}"
}

stage_slug() {
  printf '%s' "${1//[^A-Za-z0-9_.-]/_}"
}

conary="${JOURNEY_CONARY:-conary}"
bin_root="${JOURNEY_BIN_ROOT:-}"
profiles=()
declare -A row_name=() row_file=() row_binary=() row_binary_sha=()
overall_failed=0
work=""
stages_file=""

# Append one stage object to the staging document and remember the verdict.
record_stage() {
  local id="$1" passed="$2" reason="$3" exit_code="$4" stderr_tail="${5:-}"
  {
    printf '{"id":"%s","passed":%s,"exit_code":%s,"reason":"%s"' \
      "$id" "$passed" "$exit_code" "$reason"
    if [[ "$reason" == "$REASON_COMMAND_FAILED" ]]; then
      printf ',"stderr_tail":"%s"' "$(json_escape "$stderr_tail")"
    fi
    printf '}\n'
  } >> "$stages_file"
  if [[ "$passed" != "true" ]]; then
    overall_failed=1
  fi
}

# Run one command stage, capturing the last 20 stderr lines for the evidence.
run_command_stage() {
  local id="$1"
  shift
  local err_file
  err_file="$work/$(stage_slug "$id").stderr"
  local status=0 stderr_tail=""
  "$@" >/dev/null 2>"$err_file" || status=$?
  if [[ "$status" -eq 0 ]]; then
    record_stage "$id" true "$REASON_OK" 0
    return
  fi
  stderr_tail="$(tail -n 20 "$err_file")"
  record_stage "$id" false "$REASON_COMMAND_FAILED" "$status" "$stderr_tail"
}

# Prove the installed payload: present, exact digest, and executable.
run_stage() {
  local profile="$1" binary_path="$2" expected_sha="$3"
  local id="run:$profile"
  if [[ ! -e "$binary_path" ]]; then
    record_stage "$id" false "$REASON_BINARY_MISSING" 1
    return
  fi
  local actual_sha
  actual_sha="$(sha256_file "$binary_path")"
  if [[ "$actual_sha" != "$expected_sha" ]]; then
    record_stage "$id" false "$REASON_BINARY_DIGEST_MISMATCH" 1
    return
  fi
  local err_file
  err_file="$work/$(stage_slug "$id").stderr"
  local status=0 stderr_tail=""
  "$binary_path" --version >/dev/null 2>"$err_file" || status=$?
  if [[ "$status" -eq 0 ]]; then
    record_stage "$id" true "$REASON_OK" 0
    return
  fi
  stderr_tail="$(tail -n 20 "$err_file")"
  record_stage "$id" false "$REASON_EXECUTION_FAILED" "$status" "$stderr_tail"
}

# Remove the package and prove its payload is gone.
remove_stage() {
  local profile="$1" binary_path="$2" name="$3"
  local id="remove:$profile"
  local err_file
  err_file="$work/$(stage_slug "$id").stderr"
  local status=0 stderr_tail=""
  "$conary" remove "$name" --yes >/dev/null 2>"$err_file" || status=$?
  if [[ "$status" -ne 0 ]]; then
    stderr_tail="$(tail -n 20 "$err_file")"
    record_stage "$id" false "$REASON_COMMAND_FAILED" "$status" "$stderr_tail"
    return
  fi
  if [[ -e "$binary_path" ]]; then
    record_stage "$id" false "$REASON_STILL_PRESENT" 0
    return
  fi
  record_stage "$id" true "$REASON_OK" 0
}

read_packages() {
  local packages="$1" line first=1 profile
  local -a fields
  profiles=()
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ -z "$line" ]]; then
      continue
    fi
    IFS=$'\t' read -r -a fields <<<"$line"
    if [[ "$first" -eq 1 ]]; then
      first=0
      if [[ "${#fields[@]}" -ne 8 ||
            "${fields[0]}" != "profile" ||
            "${fields[1]}" != "name" ||
            "${fields[2]}" != "format" ||
            "${fields[3]}" != "file" ||
            "${fields[4]}" != "sha256" ||
            "${fields[5]}" != "url" ||
            "${fields[6]}" != "binary" ||
            "${fields[7]}" != "binary_sha256" ]]; then
        harness_error "unexpected header in $packages"
      fi
      continue
    fi
    [[ "${#fields[@]}" -eq 8 ]] || harness_error "row does not have 8 columns: $line"
    profile="${fields[0]}"
    if [[ ! "$profile" =~ ^[A-Za-z0-9._-]+$ ]]; then
      harness_error "invalid profile identifier: $profile"
    fi
    profiles+=("$profile")
    row_name["$profile"]="${fields[1]}"
    row_file["$profile"]="${fields[3]}"
    row_binary["$profile"]="${fields[6]}"
    row_binary_sha["$profile"]="${fields[7]}"
  done <"$packages"
  [[ "${#profiles[@]}" -gt 0 ]] || harness_error "no package rows in $packages"
}

host=""
packages=""
downloads=""
evidence=""

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --host)
      [[ "$#" -ge 2 ]] || usage
      host="$2"
      shift 2
      ;;
    --packages)
      [[ "$#" -ge 2 ]] || usage
      packages="$2"
      shift 2
      ;;
    --downloads)
      [[ "$#" -ge 2 ]] || usage
      downloads="$2"
      shift 2
      ;;
    --evidence)
      [[ "$#" -ge 2 ]] || usage
      evidence="$2"
      shift 2
      ;;
    *) usage ;;
  esac
done

[[ -n "$host" && -n "$packages" && -n "$downloads" && -n "$evidence" ]] || usage
[[ -f "$packages" ]] || harness_error "packages table not found: $packages"
[[ -d "$downloads" ]] || harness_error "downloads directory not found: $downloads"

work="$(mktemp -d)"
stages_file="$work/stages.jsonl"
trap 'rm -rf -- "$work"' EXIT

read_packages "$packages"

host_known=0
for profile in "${profiles[@]}"; do
  if [[ "$profile" == "$host" ]]; then
    host_known=1
  fi
done
[[ "$host_known" -eq 1 ]] || harness_error "unknown host profile: $host"

for profile in "${profiles[@]}"; do
  [[ -f "$downloads/${row_file[$profile]}" ]] ||
    harness_error "missing download for $profile: ${row_file[$profile]}"
done

conary_version=""
conary_version_present=0
if version_output="$("$conary" --version 2>/dev/null)"; then
  if [[ -n "$version_output" ]]; then
    conary_version="$version_output"
    conary_version_present=1
  fi
fi

# The host's own source profile runs first, then the remaining rows in table order.
ordered=("$host")
for profile in "${profiles[@]}"; do
  if [[ "$profile" == "$host" ]]; then
    continue
  fi
  ordered+=("$profile")
done

run_command_stage init "$conary" system init
run_command_stage adopt "$conary" system adopt --system --full

for profile in "${ordered[@]}"; do
  file="${row_file[$profile]}"
  expected_sha="${row_binary_sha[$profile]}"
  binary_path="${bin_root}${row_binary[$profile]}"

  if [[ -e "$binary_path" ]]; then
    record_stage "absent-before:$profile" false "$REASON_PREEXISTING_BINARY" 1
  else
    record_stage "absent-before:$profile" true "$REASON_OK" 0
  fi

  run_command_stage "install:$profile" "$conary" install "$downloads/$file" \
    --from "$profile" --yes
  run_stage "$profile" "$binary_path" "$expected_sha"
  remove_stage "$profile" "$binary_path" "${row_name[$profile]}"
done

mkdir -p "$(dirname "$evidence")"
staging="$work/evidence.json"
{
  printf '{"schema":"conary-user-journey-v1","host":"%s","conary_version":' "$host"
  if [[ "$conary_version_present" -eq 1 ]]; then
    printf '"%s"' "$(json_escape "$conary_version")"
  else
    printf 'null'
  fi
  printf ',"stages":['
  first_stage=1
  while IFS= read -r stage_line; do
    if [[ "$first_stage" -eq 1 ]]; then
      first_stage=0
    else
      printf ','
    fi
    printf '%s' "$stage_line"
  done <"$stages_file"
  printf ']}\n'
} >"$staging"

# jq normalises and validates the document when present; the bash escaper above
# is always the string encoder, so an escaping defect fails even with jq.
if command -v jq >/dev/null 2>&1; then
  jq . "$staging" >"$evidence" || harness_error "evidence JSON is invalid"
else
  cp "$staging" "$evidence"
fi

if [[ "$overall_failed" -ne 0 ]]; then
  exit 1
fi
exit 0
