#!/usr/bin/env bash
# scripts/test-ci-install-ubuntu-packages.sh -- Prove the hosted-Ubuntu package bootstrap's apt bounds against local mirrors.
#
# Runs scripts/ci-install-ubuntu-packages.sh inside the pinned Ubuntu 24.04
# image (the hosted ubuntu-latest release) with its canonical apt source
# pointed at host-local endpoints. No case needs the internet once the image
# is present:
#   stall-silent  accepts connections and never answers; apt's own acquire
#                 timeout and retry count (the helper's Acquire options) must
#                 fail the update with exit 100 well inside the outer bound.
#   stall-trickle answers and then sends one body byte every few seconds, so
#                 apt's inactivity timeout never fires; only the outer update
#                 bound can stop it.
#   unreachable   a closed local port; the failed index must fail the update
#                 itself, not a later install.
#   mirror        a responding unsigned local repository (positive control);
#                 the helper must install the probe package.
# The container uses host networking so 127.0.0.1 reaches the host listeners,
# and a root `sudo` shim stands in for the runner's passwordless sudo.
#
# Requires python3, dpkg-deb, and a container engine on the host. A set
# CONTAINER_ENGINE must be docker or podman; unset, the test uses docker when
# it is on PATH (the hosted-runner path), else podman.
set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

if [[ -n "${CONTAINER_ENGINE+set}" ]]; then
  engine="$CONTAINER_ENGINE"
  case "$engine" in
    docker | podman) ;;
    *)
      echo "CONTAINER_ENGINE must be docker or podman, got: '$engine'" >&2
      exit 2
      ;;
  esac
elif command -v docker >/dev/null; then
  engine=docker
elif command -v podman >/dev/null; then
  engine=podman
else
  echo "no container engine: neither docker nor podman is on PATH" >&2
  exit 2
fi
for tool in "$engine" python3 dpkg-deb timeout; do
  if ! command -v "$tool" >/dev/null; then
    echo "required host tool is missing: $tool" >&2
    exit 2
  fi
done

# Same digest as the Ubuntu 24.04 release-root base in
# apps/conary/tests/integration/remi/config.toml.
image='docker.io/library/ubuntu:24.04@sha256:561618e2c15bf2397621dd04f96926663a3b5616c189cf7e38db7e82f5c538ea'
helper=scripts/ci-install-ubuntu-packages.sh
probe_package=conary-ci-apt-probe

read_helper_seconds() {
  local name="$1"
  local value

  value="$(sed -n "s/^${name}=\([1-9][0-9]*\)\$/\1/p" "$helper")"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "${helper}: expected exactly one ${name}=<positive integer>" >&2
    exit 1
  fi
  printf '%s\n' "$value"
}
# Reads one `-o "Acquire::<name>=<n>"` element of the helper's apt_options.
read_helper_acquire_option() {
  local name="$1"
  local values

  values="$(sed -n "s/^[[:space:]]*-o \"Acquire::${name}=\([0-9][0-9]*\)\"\$/\1/p" "$helper")"
  if [[ ! "$values" =~ ^[0-9]+$ ]]; then
    echo "${helper}: expected exactly one -o \"Acquire::${name}=<integer>\" apt option" >&2
    exit 1
  fi
  printf '%s\n' "$values"
}
update_bound="$(read_helper_seconds apt_update_timeout_seconds)"
install_bound="$(read_helper_seconds apt_install_timeout_seconds)"
kill_grace="$(read_helper_seconds apt_kill_grace_seconds)"
# The local endpoints are plain http, so Acquire::http::Timeout governs them.
acquire_timeout="$(read_helper_acquire_option http::Timeout)"
acquire_retries="$(read_helper_acquire_option Retries)"
# Container start, apt list parsing, and one-second clock granularity.
margin_seconds=20
# On apt 2.8 one silent fetch costs about (Retries + 1) x 2 x Timeout seconds
# before apt reports it; the helper relies on that ending inside the update
# bound, with room to tell the two failures apart.
acquire_ceiling=$(( (acquire_retries + 1) * 2 * acquire_timeout + margin_seconds ))
if (( acquire_timeout < 1 || acquire_ceiling >= update_bound )); then
  echo "${helper}: a silent fetch may take up to ${acquire_ceiling}s (Acquire::http::Timeout=${acquire_timeout}, Acquire::Retries=${acquire_retries}), which does not end inside the ${update_bound}s update bound" >&2
  exit 1
fi
error_title='::error title=Ubuntu package bootstrap failed::'

tmpdir="$(mktemp -d)"
server_pids=()
container_names=()
cleanup() {
  local pid name
  for name in "${container_names[@]}"; do
    "$engine" rm -f "$name" >/dev/null 2>&1 || true
  done
  for pid in "${server_pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  rm -rf "$tmpdir"
}
trap cleanup EXIT

cat > "$tmpdir/mirror.py" <<'PY'
"""Host-local apt endpoints: silent, trickle, closed, or a static repository."""
import functools
import http.server
import os
import socket
import sys
import threading
import time


def write_port(path, port):
    with open(path + ".tmp", "w", encoding="ascii") as handle:
        handle.write(f"{port}\n")
    # Publish atomically so the harness never reads a partial port.
    os.replace(path + ".tmp", path)


def stall(listener, trickle):
    def serve(conn):
        try:
            if trickle:
                conn.recv(65536)
                conn.sendall(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n"
                )
                while True:
                    time.sleep(5)
                    conn.sendall(b"x")
            else:
                while conn.recv(65536):
                    pass
        except OSError:
            pass

    held = []
    while True:
        conn, _ = listener.accept()
        held.append(conn)
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


def main():
    mode, port_file = sys.argv[1], sys.argv[2]
    if mode == "repo":
        handler = functools.partial(
            http.server.SimpleHTTPRequestHandler, directory=sys.argv[3]
        )
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        write_port(port_file, server.server_address[1])
        server.serve_forever()
        return
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    if mode == "closed":
        # Reserve an ephemeral port, then release it without listening.
        port = listener.getsockname()[1]
        listener.close()
        write_port(port_file, port)
        return
    listener.listen(64)
    write_port(port_file, listener.getsockname()[1])
    stall(listener, trickle=(mode == "trickle"))


main()
PY

# Sets endpoint_port. Runs in the parent shell so cleanup owns the server.
start_endpoint() {
  local mode="$1"
  local port_file="$tmpdir/${mode}.port"
  shift

  python3 -I "$tmpdir/mirror.py" "$mode" "$port_file" "$@" \
    >"$tmpdir/${mode}.server.log" 2>&1 &
  server_pids+=("$!")
  for _ in $(seq 1 100); do
    if [[ -s "$port_file" ]]; then
      endpoint_port="$(cat "$port_file")"
      return 0
    fi
    sleep 0.1
  done
  echo "local ${mode} endpoint did not publish its port" >&2
  cat "$tmpdir/${mode}.server.log" >&2
  exit 1
}

# Build the positive-control repository: one architecture-independent probe
# package in an unsigned flat repository trusted through its source entry.
repo_dir="$tmpdir/repo"
pkg_root="$tmpdir/pkg"
mkdir -p "$repo_dir" "$pkg_root/DEBIAN" "$pkg_root/usr/share/${probe_package}"
printf 'installed by the bounded apt runtime test\n' \
  > "$pkg_root/usr/share/${probe_package}/marker"
cat > "$pkg_root/DEBIAN/control" <<EOF
Package: ${probe_package}
Version: 1.0
Architecture: all
Maintainer: Conary CI <ci@invalid>
Description: Conary CI apt bound probe
EOF
deb_name="${probe_package}_1.0_all.deb"
dpkg-deb --root-owner-group -Zgzip --build "$pkg_root" "$repo_dir/$deb_name" >/dev/null
deb_size="$(stat -c %s "$repo_dir/$deb_name")"
deb_sha256="$(sha256sum "$repo_dir/$deb_name" | cut -d' ' -f1)"
{
  dpkg-deb --field "$repo_dir/$deb_name"
  printf 'Filename: ./%s\nSize: %s\nSHA256: %s\n\n' \
    "$deb_name" "$deb_size" "$deb_sha256"
} > "$repo_dir/Packages"
packages_size="$(stat -c %s "$repo_dir/Packages")"
packages_sha256="$(sha256sum "$repo_dir/Packages" | cut -d' ' -f1)"
cat > "$repo_dir/Release" <<EOF
Origin: conary-ci
Label: conary-ci
Date: $(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')
Architectures: amd64 all
SHA256:
 ${packages_sha256} ${packages_size} Packages
EOF

# The harness directory is mounted read-only; everything the container
# writes stays inside the container.
harness="$tmpdir/harness"
mkdir -p "$harness"
cp "$helper" "$harness/ci-install-ubuntu-packages.sh"
cat > "$harness/sudo" <<'EOF'
#!/bin/sh
# The harness container already runs as root.
exec "$@"
EOF
cat > "$harness/entry.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
install -m 0755 /harness/sudo /usr/local/bin/sudo
cp "/harness/\$1.sources" /etc/apt/sources.list.d/ubuntu.sources
started="\$(date +%s)"
status=0
bash /harness/ci-install-ubuntu-packages.sh ${probe_package} || status=\$?
elapsed=\$(( \$(date +%s) - started ))
installed=no
if [[ "\$(dpkg-query --show --showformat='\${Status}' ${probe_package} 2>/dev/null || true)" == "install ok installed" ]]; then
  installed=yes
fi
echo "bounded-apt-result status=\$status elapsed=\$elapsed installed=\$installed"
EOF

write_sources() {
  local name="$1"
  local uri="$2"
  local suites="$3"
  local components="$4"

  {
    printf 'Types: deb\nURIs: %s\nSuites: %s\n' "$uri" "$suites"
    if [[ -n "$components" ]]; then
      printf 'Components: %s\n' "$components"
    fi
    printf 'Trusted: yes\n'
  } > "$harness/${name}.sources"
}

start_endpoint silent
write_sources stall-silent "http://127.0.0.1:${endpoint_port}/ubuntu/" noble main
start_endpoint trickle
write_sources stall-trickle "http://127.0.0.1:${endpoint_port}/ubuntu/" noble main
start_endpoint closed
write_sources unreachable "http://127.0.0.1:${endpoint_port}/ubuntu/" noble main
start_endpoint repo "$repo_dir"
write_sources mirror "http://127.0.0.1:${endpoint_port}/" ./ ""

# The pinned image is the test's only network fetch; bound it so a stalled
# registry fails here with its own message instead of as a job timeout.
pull_timeout_seconds=300
if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
  pull_status=0
  timeout --kill-after=10s "${pull_timeout_seconds}s" \
    "$engine" pull "$image" >/dev/null || pull_status=$?
  if [[ "$pull_status" -eq 124 || "$pull_status" -eq 137 ]]; then
    echo "[fail] ${engine} pull of ${image} did not finish within ${pull_timeout_seconds}s (timeout exit ${pull_status})" >&2
    exit 1
  elif [[ "$pull_status" -ne 0 ]]; then
    echo "[fail] ${engine} pull of ${image} failed with exit status ${pull_status}" >&2
    exit 1
  fi
fi

# Per-case watchdogs; a case that reaches its watchdog has lost the helper's
# own bound. The stall cases get a full update and install. unreachable and
# mirror must finish inside the update bound (their assertions require it),
# so they get the update bound only, which keeps the whole test inside the
# workflow-runtime-policy job's timeout even when every case hangs.
full_watchdog_seconds=$(( update_bound + install_bound + 2 * kill_grace + margin_seconds ))
fast_watchdog_seconds=$(( update_bound + kill_grace + margin_seconds ))
case_watchdog_seconds() {
  case "$1" in
    stall-silent | stall-trickle) echo "$full_watchdog_seconds" ;;
    unreachable | mirror) echo "$fast_watchdog_seconds" ;;
    *)
      echo "no watchdog for case: $1" >&2
      exit 1
      ;;
  esac
}

case_names=(stall-silent stall-trickle unreachable mirror)
for name in "${case_names[@]}"; do
  container_names+=("conary-bounded-apt-$$-${name}")
done

run_case() {
  local name="$1"
  local container="conary-bounded-apt-$$-${name}"
  local status=0
  local watchdog

  watchdog="$(case_watchdog_seconds "$name")"
  timeout --kill-after=10s "${watchdog}s" \
    "$engine" run --rm --name "$container" --network host \
    -v "$harness:/harness:ro" \
    "$image" bash /harness/entry.sh "$name" \
    >"$tmpdir/${name}.log" 2>&1 || status=$?
  echo "$status" > "$tmpdir/${name}.engine-status"
}

failures=0
fail_case() {
  local name="$1"
  local reason="$2"

  echo "[fail] ${name}: ${reason}" >&2
  sed 's/^/    /' "$tmpdir/${name}.log" >&2
  failures=$(( failures + 1 ))
}

# Sets case_status, case_elapsed, and case_installed from the result line.
read_case_result() {
  local name="$1"
  local engine_status line

  engine_status="$(cat "$tmpdir/${name}.engine-status")"
  if [[ "$engine_status" -ne 0 ]]; then
    fail_case "$name" "container run exited ${engine_status} (watchdog $(case_watchdog_seconds "$name")s)"
    return 1
  fi
  line="$(grep -E '^bounded-apt-result ' "$tmpdir/${name}.log" | tail -n 1 || true)"
  if [[ ! "$line" =~ ^bounded-apt-result\ status=([0-9]+)\ elapsed=([0-9]+)\ installed=(yes|no)$ ]]; then
    fail_case "$name" "missing harness result line"
    return 1
  fi
  case_status="${BASH_REMATCH[1]}"
  case_elapsed="${BASH_REMATCH[2]}"
  case_installed="${BASH_REMATCH[3]}"
}

assert_no_downstream_install_error() {
  local name="$1"

  if grep -qF 'Unable to locate package' "$tmpdir/${name}.log"; then
    fail_case "$name" "failure surfaced as a downstream install error"
    return 1
  fi
}

# stall-silent: apt's own acquire timeout must end the update, so the helper
# reports apt's failure, not the outer bound. The floor proves the endpoint
# actually stalled: a dead listener refuses at once with the same exit 100.
assert_acquire_failure() {
  local name="$1"
  local expected="${error_title}apt-get update failed with exit status 100 within its ${update_bound}-second bound"
  local timeout_message="${error_title}apt-get update did not finish"

  read_case_result "$name" || return 0
  if [[ "$case_status" -ne 1 ]]; then
    fail_case "$name" "helper exited ${case_status}, expected 1"
  elif (( case_elapsed < acquire_timeout || case_elapsed > acquire_ceiling )); then
    fail_case "$name" "elapsed ${case_elapsed}s, expected ${acquire_timeout}..${acquire_ceiling}s from Acquire::http::Timeout=${acquire_timeout} and Acquire::Retries=${acquire_retries}"
  elif grep -qF -- "$timeout_message" "$tmpdir/${name}.log"; then
    fail_case "$name" "the outer update bound stopped apt instead of its acquire timeout"
  elif ! grep -qF -- "$expected" "$tmpdir/${name}.log"; then
    fail_case "$name" "missing helper message: ${expected}"
  elif assert_no_downstream_install_error "$name"; then
    echo "[ok] ${name}: apt's acquire timeout failed the update after ${case_elapsed}s (ceiling ${acquire_ceiling}s)"
  fi
}

assert_update_timeout() {
  local name="$1"
  local expected="${error_title}apt-get update did not finish within its ${update_bound}-second bound"
  local ceiling=$(( update_bound + kill_grace + margin_seconds ))

  read_case_result "$name" || return 0
  if [[ "$case_status" -ne 1 ]]; then
    fail_case "$name" "helper exited ${case_status}, expected 1"
  elif (( case_elapsed < update_bound || case_elapsed > ceiling )); then
    fail_case "$name" "elapsed ${case_elapsed}s, expected ${update_bound}..${ceiling}s"
  elif ! grep -qF -- "$expected" "$tmpdir/${name}.log"; then
    fail_case "$name" "missing helper message: ${expected}"
  elif assert_no_downstream_install_error "$name"; then
    echo "[ok] ${name}: update stopped by its ${update_bound}s bound after ${case_elapsed}s"
  fi
}

# The stall cases wait out apt's acquire timeout and the full update bound,
# so run them concurrently with the fast cases.
run_case stall-silent &
silent_job=$!
run_case stall-trickle &
trickle_job=$!

run_case unreachable
expected="${error_title}apt-get update failed with exit status 100 within its ${update_bound}-second bound"
if read_case_result unreachable; then
  if [[ "$case_status" -ne 1 ]]; then
    fail_case unreachable "helper exited ${case_status}, expected 1"
  elif (( case_elapsed >= update_bound )); then
    fail_case unreachable "elapsed ${case_elapsed}s reached the ${update_bound}s update bound"
  elif ! grep -qF -- "$expected" "$tmpdir/unreachable.log"; then
    fail_case unreachable "missing helper message: ${expected}"
  elif assert_no_downstream_install_error unreachable; then
    echo "[ok] unreachable: failed index rejected by update after ${case_elapsed}s"
  fi
fi

run_case mirror
if read_case_result mirror; then
  if [[ "$case_status" -ne 0 ]]; then
    fail_case mirror "helper exited ${case_status}, expected 0"
  elif [[ "$case_installed" != yes ]]; then
    fail_case mirror "${probe_package} is not installed"
  elif (( case_elapsed >= update_bound )); then
    fail_case mirror "elapsed ${case_elapsed}s reached the ${update_bound}s update bound"
  elif grep -qF -- "$error_title" "$tmpdir/mirror.log"; then
    fail_case mirror "helper reported a failure on a responding mirror"
  else
    echo "[ok] mirror: installed ${probe_package} from the local mirror in ${case_elapsed}s"
  fi
fi

wait "$silent_job" || true
wait "$trickle_job" || true
assert_acquire_failure stall-silent
assert_update_timeout stall-trickle

if [[ "$failures" -ne 0 ]]; then
  echo "bounded apt runtime test: ${failures} case(s) failed" >&2
  exit 1
fi
echo "bounded apt runtime test passed."
