#!/usr/bin/env python3
"""Prepare the pinned Mozilla sccache archive for the runner tool cache.

Stdlib-only helper for action.yml (issue #680). `describe` reports the exact
archive cache path/key; `prepare` restores or downloads the pinned archive,
verifies its digest before extraction, and preseeds the pinned
tool-cache version/arch with the verified binary and marker. Downloaded or
restored bytes never enter PATH.
"""

from __future__ import annotations

import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

PIN_FILE = "pin.json"
CACHE_SUBDIR = "conary-sccache-archive"
DOWNLOAD_TIMEOUT_SECONDS = 120
MAX_ARCHIVE_BYTES = 64 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class PrepareError(RuntimeError):
    pass


def load_pin(path: str | None = None) -> dict:
    path = path or os.path.join(os.path.dirname(os.path.abspath(__file__)), PIN_FILE)
    with open(path, "r", encoding="utf-8") as handle:
        pin = json.load(handle)
    if set(pin) != {"version", "target", "sha256"}:
        raise PrepareError("pin must contain exactly version, target and sha256")
    if not all(isinstance(value, str) for value in pin.values()):
        raise PrepareError("pin values must be strings")
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", pin["version"]):
        raise PrepareError("pin version must be an exact release version")
    if pin["target"] != "x86_64-unknown-linux-musl" or not SHA256_RE.fullmatch(pin["sha256"]):
        raise PrepareError("pin must select Linux x64 with a lowercase SHA-256")
    name = f"sccache-v{pin['version']}-{pin['target']}"
    pin["url"] = f"https://github.com/mozilla/sccache/releases/download/v{pin['version']}/{name}.tar.gz"
    pin["archive_member"] = f"{name}/sccache"
    return pin


def archive_cache_path(runner_temp: str, pin: dict) -> str:
    name = f"sccache-v{pin['version']}-{pin['target']}.tar.gz"
    return os.path.join(runner_temp, CACHE_SUBDIR, name)


def cache_key(pin: dict) -> str:
    return f"conary-sccache-{pin['target']}-v{pin['version']}-{pin['sha256']}"


def tool_cache_dir(tool_cache: str, pin: dict) -> str:
    return os.path.join(tool_cache, "sccache",
                        pin["version"], "x64")


def sha256_file(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_archive(path: str, expected_sha256: str) -> None:
    if os.path.islink(path) or not os.path.isfile(path):
        raise PrepareError(f"pinned archive is not a regular file: {path}")
    if os.path.getsize(path) > MAX_ARCHIVE_BYTES:
        raise PrepareError("pinned archive exceeds size bound")
    actual = sha256_file(path)
    if actual != expected_sha256:
        raise PrepareError(f"pinned archive digest mismatch at {path}: "
                           f"expected {expected_sha256}, got {actual}")


def _curl_download(url: str, destination: str) -> None:
    command = ["curl", "--fail", "--silent", "--show-error", "--location",
               "--proto", "=https", "--proto-redir", "=https",
               "--max-time", str(DOWNLOAD_TIMEOUT_SECONDS),
               "--max-filesize", str(MAX_ARCHIVE_BYTES), "--output", destination, url]
    try:
        subprocess.run(command, check=True, timeout=DOWNLOAD_TIMEOUT_SECONDS + 5)
    except (OSError, subprocess.SubprocessError) as error:
        raise PrepareError(f"bounded archive download failed: {error}") from error


def download_archive(pin: dict, destination: str, download=None) -> None:
    directory = os.path.dirname(destination)
    os.makedirs(directory, exist_ok=True)
    if os.path.lexists(destination):
        verify_archive(destination, pin["sha256"])
        return
    with tempfile.TemporaryDirectory(prefix="download-", dir=directory) as work:
        partial = os.path.join(work, "archive.tar.gz")
        (download or _curl_download)(pin["url"], partial)
        verify_archive(partial, pin["sha256"])
        os.replace(partial, destination)


def extract_member(archive_path: str, member_name: str, destination: str) -> None:
    """Extract only the expected regular member; never extractall."""
    with tarfile.open(archive_path, "r:gz") as archive:
        matches = [m for m in archive.getmembers() if m.name == member_name]
        if len(matches) != 1:
            raise PrepareError(f"expected exactly one {member_name!r} member, "
                               f"found {len(matches)}")
        member = matches[0]
        if not member.isreg():
            raise PrepareError(f"expected member {member_name!r} is not a regular file")
        source = archive.extractfile(member)
        if source is None:
            raise PrepareError(f"cannot read expected member {member_name!r}")
        with open(destination, "wb") as handle:
            shutil.copyfileobj(source, handle)
    os.chmod(destination, 0o755)


def install_tool_cache(tool_cache: str, pin: dict, verified_binary: str) -> str:
    target_dir = tool_cache_dir(tool_cache, pin)
    version_dir = os.path.dirname(target_dir)
    marker = f"{target_dir}.complete"
    for path in (tool_cache, os.path.join(tool_cache, "sccache"), version_dir,
                 target_dir, marker):
        if os.path.islink(path):
            raise PrepareError(f"refusing symlinked tool-cache path: {path}")
    if os.path.exists(target_dir) and not os.path.isdir(target_dir):
        raise PrepareError(f"refusing non-directory tool-cache path: {target_dir}")
    os.makedirs(version_dir, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".prepare-", dir=version_dir) as work:
        staging = os.path.join(work, "x64")
        os.mkdir(staging)
        binary = os.path.join(staging, "sccache")
        shutil.copyfile(verified_binary, binary)
        os.chmod(binary, 0o755)
        # tool-cache find() requires a sibling marker, never one inside PATH.
        # Remove authority before replacement; failure cannot leave a stale hit.
        if os.path.exists(marker):
            os.unlink(marker)
        if os.path.exists(target_dir):
            shutil.rmtree(target_dir)
        os.rename(staging, target_dir)
        with open(marker, "x", encoding="utf-8") as handle:
            handle.write("")
    return target_dir


def prepare(pin: dict, *, runner_temp: str, tool_cache: str, cache_hit: bool,
            download=None) -> dict:
    archive = archive_cache_path(runner_temp, pin)
    cache_dir = os.path.dirname(archive)
    if os.path.islink(runner_temp) or os.path.islink(cache_dir):
        raise PrepareError(f"refusing symlinked archive cache directory: {cache_dir}")
    if cache_hit:
        # A corrupt restored archive fails here and is never redownloaded.
        verify_archive(archive, pin["sha256"])
    else:
        download_archive(pin, archive, download=download)
    work = tempfile.mkdtemp(prefix="sccache-verify-", dir=runner_temp)
    try:
        binary = os.path.join(work, "sccache")
        extract_member(archive, pin["archive_member"], binary)
        installed = install_tool_cache(tool_cache, pin, binary)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    return {"archive_path": archive, "cache_key": cache_key(pin),
            "tool_cache_dir": installed}


def _require(name: str) -> str:
    value = os.environ.get(name, "")
    if not value:
        raise PrepareError(f"required environment variable {name} is not set")
    return value


def main(argv: list[str]) -> int:
    mode = argv[1] if len(argv) > 1 else ""
    try:
        if sys.platform != "linux" or platform.machine().lower() not in ("x86_64", "amd64"):
            raise PrepareError(f"unsupported platform: {sys.platform}/{platform.machine()}")
        pin = load_pin()
        runner_temp = _require("RUNNER_TEMP")
        if mode == "describe":
            with open(_require("GITHUB_OUTPUT"), "a", encoding="utf-8") as handle:
                handle.write(f"cache_path={archive_cache_path(runner_temp, pin)}\n")
                handle.write(f"cache_key={cache_key(pin)}\n")
        elif mode == "prepare":
            result = prepare(
                pin, runner_temp=runner_temp, tool_cache=_require("RUNNER_TOOL_CACHE"),
                cache_hit=os.environ.get("SCCACHE_ARCHIVE_CACHE_HIT", "") == "true")
            print(f"[ok] preseeded {result['tool_cache_dir']} "
                  f"from {result['archive_path']}")
        else:
            raise PrepareError(f"unknown mode {mode!r}; expected describe or prepare")
    except (PrepareError, OSError, tarfile.TarError, ValueError) as error:
        print(f"[fail] prepare-sccache: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
