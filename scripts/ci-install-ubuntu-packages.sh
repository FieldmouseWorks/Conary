#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: $0 PACKAGE [PACKAGE ...]" >&2
  exit 2
fi

missing_packages=()
for package in "$@"; do
  if [[ ! "$package" =~ ^[a-z0-9][a-z0-9+.-]*$ ]]; then
    echo "invalid Ubuntu package name: $package" >&2
    exit 2
  fi

  status="$(
    dpkg-query --show --showformat='${Status}' "$package" 2>/dev/null || true
  )"
  if [[ "$status" != "install ok installed" ]]; then
    missing_packages+=("$package")
  fi
done

if [[ "${#missing_packages[@]}" -eq 0 ]]; then
  echo "Requested Ubuntu packages are already installed: $*"
  exit 0
fi

ubuntu_sources=/etc/apt/sources.list.d/ubuntu.sources
if [[ ! -f "$ubuntu_sources" || -L "$ubuntu_sources" ]]; then
  echo "canonical Ubuntu apt source is not a plain file: $ubuntu_sources" >&2
  exit 1
fi

# Bounded apt contract. Each apt-get call runs under an outer `timeout` that
# root owns, so expiry kills apt-get and its fetch methods (SIGTERM, then
# SIGKILL after the grace period). The outer bound is the hard limit: apt's
# own inactivity timeout restarts on every received byte, so a mirror that
# trickles data is stopped only by it. The acquire options pin apt's
# per-fetch inactivity timeout and retry count to the hosted runner's own
# mirror-failover values (15 seconds, 1 retry), so they hold even where no
# runner apt.conf.d sets them. On apt 2.8 one silent fetch costs about
# (Retries + 1) x 2 x Timeout seconds (about 60) before apt reports it,
# inside the update bound.
#
# Budget: update + install bounds plus both kill grace periods total 410
# seconds, leaving more than three minutes of the smallest consuming job
# budget (ci-base-image-policy, timeout-minutes: 10) for checkout and the
# job's own proof, so a stalled mirror fails here with its own message
# instead of as a job cancellation.
#
# `--error-on=any` makes a failed index fetch an update failure instead of a
# warning followed by installation from incomplete indexes. Failures are not
# retried until green: a fetch that cannot complete within its bound is a
# reported CI infrastructure failure.
apt_update_timeout_seconds=150
apt_install_timeout_seconds=240
apt_kill_grace_seconds=10
apt_options=(
  -o "Dir::Etc::sourcelist=${ubuntu_sources}"
  -o "Dir::Etc::sourceparts=/dev/null"
  -o "Acquire::http::Timeout=15"
  -o "Acquire::https::Timeout=15"
  -o "Acquire::Retries=1"
)

report_apt_failure() {
  local step="$1"
  local bound="$2"
  local status="$3"
  local title="::error title=Ubuntu package bootstrap failed::"

  if [[ "$status" -eq 124 || "$status" -eq 137 ]]; then
    echo "${title}${step} did not finish within its ${bound}-second bound (timeout exit ${status}). This is a CI infrastructure failure: the Ubuntu mirror did not complete the transfer in time." >&2
  elif [[ "$step" == "apt-get update" ]]; then
    echo "${title}${step} failed with exit status ${status} within its ${bound}-second bound. A failed index fetch is a CI infrastructure (Ubuntu mirror) failure; the apt output above names the failed index." >&2
  else
    echo "${title}${step} failed with exit status ${status} within its ${bound}-second bound. A fetch error above is a CI infrastructure (Ubuntu mirror) failure; an unknown package is a caller error." >&2
  fi
  exit 1
}

update_status=0
sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_update_timeout_seconds}s" \
  apt-get "${apt_options[@]}" update --error-on=any || update_status=$?
if [[ "$update_status" -ne 0 ]]; then
  report_apt_failure "apt-get update" "$apt_update_timeout_seconds" "$update_status"
fi

install_status=0
sudo timeout --kill-after="${apt_kill_grace_seconds}s" "${apt_install_timeout_seconds}s" \
  env DEBIAN_FRONTEND=noninteractive \
  apt-get "${apt_options[@]}" install -y --no-install-recommends \
  "${missing_packages[@]}" || install_status=$?
if [[ "$install_status" -ne 0 ]]; then
  report_apt_failure "apt-get install" "$apt_install_timeout_seconds" "$install_status"
fi
