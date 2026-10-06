#!/usr/bin/env bash
# scripts/user-journey-fetch.sh
#
# Download and digest-verify the pinned upstream packages consumed by
# scripts/user-journey.sh. Runs on the CI runner, outside the container.
# Download and digest failures are harness failures (exit 2); they never
# report a product verdict.
set -euo pipefail

usage() {
  echo "usage: $0 <packages.tsv> <download-dir>" >&2
  exit 2
}

harness_error() {
  echo "user-journey-fetch harness error: $*" >&2
  exit 2
}

sha256_file() {
  local digest
  digest="$(sha256sum "$1")"
  printf '%s' "${digest%% *}"
}

[[ "$#" -eq 2 ]] || usage
packages="$1"
downloads="$2"

[[ -f "$packages" ]] || harness_error "packages table not found: $packages"
mkdir -p "$downloads"

first=1
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
  file="${fields[3]}"
  expected_sha="${fields[4]}"
  url="${fields[5]}"
  destination="$downloads/$file"
  if ! curl --silent --show-error --fail --location --retry 3 --proto '=https,http' \
    --output "$destination" "$url"; then
    harness_error "download failed: $url"
  fi
  actual_sha="$(sha256_file "$destination")"
  if [[ "$actual_sha" != "$expected_sha" ]]; then
    harness_error "digest mismatch for $file: expected $expected_sha, got $actual_sha"
  fi
done < "$packages"

echo "verified pinned packages in $downloads"
