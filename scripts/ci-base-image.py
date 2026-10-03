#!/usr/bin/env python3
"""Restore reviewed release bytes into a job-local registry by pinned SHA-256."""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit


CATALOG = Path(__file__).with_name("ci-base-images.json")
HEX = re.compile(r"[0-9a-f]{64}")
VERSION_BYTES = b"Directory Transport Version: 1.1\n"
MANIFESTS = {
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
}
CONFIGS = {
    "application/vnd.docker.container.image.v1+json",
    "application/vnd.oci.image.config.v1+json",
}
LAYERS = {
    "application/vnd.docker.image.rootfs.diff.tar.gzip",
    "application/vnd.oci.image.layer.v1.tar",
    "application/vnd.oci.image.layer.v1.tar+gzip",
    "application/vnd.oci.image.layer.v1.tar+zstd",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def reference(value):
    """Parse one explicit registry/repository[:tag]@sha256 identity."""
    require(isinstance(value, str) and value.count("@sha256:") == 1,
            "image reference must have one SHA-256 pin")
    name, digest = value.split("@sha256:")
    require(HEX.fullmatch(digest), "invalid image digest")
    require(re.fullmatch(r"[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+(?::[A-Za-z0-9_.-]+)?", name),
            "expected explicit registry/repository[:tag]")
    registry, path = name.split("/", 1)
    require("." in registry or ":" in registry or registry == "localhost",
            "implicit Docker Hub registry is not an explicit authority")
    repository = path.split(":", 1)[0]
    require(all(part not in ("", ".", "..") for part in repository.split("/")),
            "invalid repository component")
    return f"{registry}/{repository}", digest


def catalog(path):
    data = json.loads(path.read_text())
    require(isinstance(data, dict) and set(data) == {"version", "images"}
            and type(data["version"]) is int and data["version"] == 2,
            "unsupported base-image catalog")
    require(isinstance(data["images"], dict) and data["images"], "empty image catalog")
    seen = set()
    for entry in data["images"].values():
        require(isinstance(entry, dict) and set(entry) == {"source", "local", "platform", "release"},
                "invalid image entry")
        source, digest = reference(entry["source"])
        local, local_digest = reference(entry["local"])
        require(digest == local_digest and source != local,
                "local transport must preserve the source digest at a distinct repository")
        require(local.startswith("127.0.0.1:") and 0 < int(local.split(":", 1)[1].split("/", 1)[0]) < 65536,
                "local transport must use a literal loopback registry port")
        require(entry["platform"] == "linux/amd64", "only reviewed linux/amd64 manifests are supported")
        release = entry["release"]
        require(isinstance(release, dict)
                and set(release) == {"repository", "tag", "asset", "sha256", "size"},
                "invalid release authority")
        require(isinstance(release["repository"], str)
                and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", release["repository"]),
                "invalid release repository")
        require(isinstance(release["tag"], str)
                and re.fullmatch(r"[A-Za-z0-9_.-]+", release["tag"]), "invalid release tag")
        require(isinstance(release["asset"], str)
                and re.fullmatch(r"[A-Za-z0-9_.-]+\.tar", release["asset"]), "invalid release asset")
        require(isinstance(release["sha256"], str) and HEX.fullmatch(release["sha256"]),
                "invalid release archive digest")
        require(type(release["size"]) is int and 0 < release["size"] <= 4 * 1024**3,
                "invalid release archive size")
        require(entry["local"] not in seen, "duplicate local reference")
        seen.add(entry["local"])
    return data["images"]


def resolve(value, entries):
    repository, digest = reference(value)
    for entry in entries.values():
        require((repository, digest) != reference(entry["source"]),
                "release-backed image must be consumed through its local reference")
        if (repository, digest) == reference(entry["local"]):
            return f"{repository}@sha256:{digest}"
    require(not repository.startswith("127.0.0.1:"),
            "local image is not registered in the reviewed catalog")
    return f"{repository}@sha256:{digest}"


def checked_file(path, digest, size=None, limit=2 * 1024**3):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode), f"not a regular blob: {path.name}")
    require(0 <= info.st_size <= limit and (size is None or info.st_size == size),
            f"blob size mismatch: {path.name}")
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            hasher.update(chunk)
    require(hasher.hexdigest() == digest, f"blob digest mismatch: {path.name}")
    return info.st_size


def verify(layout, entry):
    require(layout.is_dir() and not layout.is_symlink(), "expected a real image directory")
    version_path = layout / "version"
    require(version_path.exists() or version_path.is_symlink(),
            "missing directory transport version")
    version_info = version_path.lstat()
    require(stat.S_ISREG(version_info.st_mode) and version_info.st_size == len(VERSION_BYTES)
            and version_path.read_bytes() == VERSION_BYTES,
            "invalid directory transport version")
    _, digest = reference(entry["source"])
    manifest_path = layout / "manifest.json"
    manifest_size = checked_file(manifest_path, digest, limit=1024**2)
    manifest = json.loads(manifest_path.read_bytes())
    require(manifest.get("schemaVersion") == 2 and manifest.get("mediaType") in MANIFESTS,
            "expected a pinned platform manifest, not an index or legacy manifest")
    require(isinstance(manifest.get("layers"), list) and len(manifest["layers"]) <= 128,
            "invalid layer list")
    descriptors = [(manifest.get("config", {}), CONFIGS)]
    descriptors += [(layer, LAYERS) for layer in manifest["layers"]]
    blobs = []
    for index, (descriptor, allowed) in enumerate(descriptors):
        require(isinstance(descriptor, dict) and descriptor.get("mediaType") in allowed,
                "unsupported blob media type")
        require(not descriptor.get("urls"), "external layer URLs are not reviewed authority")
        value = descriptor.get("digest", "")
        require(value.startswith("sha256:") and HEX.fullmatch(value[7:]), "invalid blob digest")
        size = descriptor.get("size")
        require(type(size) is int and size >= 0, "invalid blob size")
        checked_file(layout / value[7:], value[7:], size,
                     limit=16 * 1024**2 if index == 0 else 2 * 1024**3)
        blobs.append({"digest": value, "bytes": size})
    config = json.loads((layout / manifest["config"]["digest"][7:]).read_bytes())
    require(f"{config.get('os')}/{config.get('architecture')}" == entry["platform"],
            "image platform does not match its reviewed authority")
    allowed_files = {"manifest.json", "version", *(blob["digest"][7:] for blob in blobs)}
    require(all(path.name in allowed_files and stat.S_ISREG(path.lstat().st_mode)
                for path in layout.iterdir()), "unexpected image-directory content")
    return {"version": 1, **entry, "manifest_sha256": digest,
            "manifest_bytes": manifest_size, "blobs": blobs}


def restore(archive, layout, entry):
    """Restore only a flat OCI dir archive, then check its pinned source bytes."""
    require(not layout.exists() and not layout.is_symlink(),
            "output already exists; use an empty destination")
    info = archive.lstat()
    require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= 4 * 1024**3
            and info.st_size % 512 == 0,
            "expected a regular OCI dir archive of bounded size")
    with tempfile.TemporaryDirectory(prefix="conary-ci-base-", dir=layout.parent) as tmp:
        staged = Path(tmp) / "image"
        staged.mkdir()
        names = set()
        with tarfile.open(archive, mode="r:") as stream:
            for member in stream:
                name = member.name
                require(name not in names, f"duplicate archive member: {name}")
                names.add(name)
                require(name in ("manifest.json", "version") or HEX.fullmatch(name),
                        f"unexpected archive member: {name}")
                require(member.isfile(), f"non-regular archive member: {name}")
                limit = 1024 if name == "version" else (1024**2 if name == "manifest.json"
                                                         else 2 * 1024**3)
                require(0 <= member.size <= limit, f"archive member too large: {name}")
                source = stream.extractfile(member)
                require(source is not None, f"unreadable archive member: {name}")
                with source, (staged / name).open("xb") as destination:
                    shutil.copyfileobj(source, destination, 1024 * 1024)
            trailer_size = info.st_size - stream.offset
            require(1024 <= trailer_size <= 10240, "invalid archive trailer")
            with archive.open("rb") as raw:
                raw.seek(stream.offset)
                require(not any(raw.read()), "nonzero bytes after archive members")
        receipt = verify(staged, entry)
        staged.rename(layout)
        return receipt


def transport_flags(value, direction, allow_http):
    if not allow_http:
        return []
    repository, _ = reference(value)
    host = urlsplit("http://" + repository).hostname
    require(ipaddress.ip_address(host).is_loopback,
            "HTTP fixture transport requires a literal loopback registry")
    return [f"--{direction}-tls-verify=false"]


def copy_image(source, destination, flags):
    subprocess.run(["skopeo", "copy", "--all", "--preserve-digests", *flags,
                    source, destination], stdout=sys.stderr, check=True, timeout=600)


def stage(entry, layout, allow_http=False):
    require(not layout.exists(), "output already exists; use an empty destination")
    value = entry["source"]
    repository, digest = reference(value)
    # Source acquisition is diagnostic; the release asset is consumer authority.
    flags = ["--src-no-creds", *transport_flags(value, "src", allow_http)]
    copy_image(f"docker://{repository}@sha256:{digest}", f"dir:{layout}", flags)
    return verify(layout, entry)


def release_urls(release, api_base="https://api.github.com", download_base="https://github.com"):
    owner_repo = release["repository"]
    tag = release["tag"]
    asset = release["asset"]
    return (f"{api_base}/repos/{owner_repo}/releases/tags/{tag}",
            f"{download_base}/{owner_repo}/releases/download/{tag}/{asset}")


def download_release(entry, archive, api_base="https://api.github.com",
                     download_base="https://github.com"):
    """Require an immutable public release and download its one pinned asset anonymously."""
    require(not archive.exists() and not archive.is_symlink(), "archive output already exists")
    release = entry["release"]
    metadata_url, asset_url = release_urls(release, api_base, download_base)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    request = urllib.request.Request(metadata_url, headers={
        "Accept": "application/vnd.github+json", "User-Agent": "Conary-CI-base-image"})
    with opener.open(request, timeout=30) as response:
        raw = response.read(2 * 1024**2 + 1)
    require(len(raw) <= 2 * 1024**2, "release metadata is too large")
    metadata = json.loads(raw)
    require(isinstance(metadata, dict) and metadata.get("tag_name") == release["tag"]
            and metadata.get("immutable") is True and metadata.get("draft") is False
            and metadata.get("prerelease") is False, "release is not the immutable reviewed release")
    assets = metadata.get("assets")
    require(isinstance(assets, list), "release has no asset list")
    matching = [asset for asset in assets if isinstance(asset, dict)
                and asset.get("name") == release["asset"]]
    require(len(matching) == 1, "release must contain exactly one named asset")
    asset = matching[0]
    require(asset.get("digest") == f"sha256:{release['sha256']}"
            and asset.get("size") == release["size"] and asset.get("state") == "uploaded"
            and asset.get("browser_download_url") == asset_url,
            "release asset metadata does not match its reviewed authority")
    hasher = hashlib.sha256()
    size = 0
    try:
        with opener.open(urllib.request.Request(asset_url, headers={
                "User-Agent": "Conary-CI-base-image"}), timeout=120) as response, archive.open("xb") as output:
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                require(size <= release["size"], "release archive exceeds its pinned size")
                hasher.update(chunk)
                output.write(chunk)
        require(size == release["size"] and hasher.hexdigest() == release["sha256"],
                "release archive size or SHA-256 mismatch")
    except BaseException:
        archive.unlink(missing_ok=True)
        raise
    return {"archive_sha256": hasher.hexdigest(), "archive_bytes": size}


def local_endpoint(entry):
    repository, digest = reference(entry["local"])
    host, path = repository.split("/", 1)
    return host, path, digest


def registry_process_matches(pid, config):
    cmdline = Path(f"/proc/{pid}/cmdline")
    try:
        command = cmdline.read_bytes().replace(b"\0", b" ")
    except OSError:
        return False
    return b"docker-registry serve" in command and str(config).encode() in command


def start_registry(entry, workdir):
    host, _, _ = local_endpoint(entry)
    require(shutil.which("docker-registry") is not None, "docker-registry is required")
    require(shutil.which("skopeo") is not None, "skopeo is required")
    # The typed base_image uses this literal port. Never adopt an existing listener.
    import socket
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", int(host.rsplit(":", 1)[1])))
    config = workdir / "registry.json"
    config.write_text(json.dumps({"version": 0.1, "log": {"level": "error"},
                                  "storage": {"filesystem": {"rootdirectory": str(workdir / "store")}},
                                  "http": {"addr": host, "secret": os.urandom(32).hex()}}))
    log = workdir / "registry.log"
    launched = subprocess.run(["bash", "-c",
                               'nohup "$1" serve "$2" </dev/null >"$3" 2>&1 & printf "%s\\n" "$!"',
                               "registry-launch", "docker-registry", str(config), str(log)],
                              capture_output=True, text=True, check=True, timeout=5)
    pid = int(launched.stdout.strip())
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + 10
    while True:
        try:
            with opener.open(f"http://{host}/v2/", timeout=1) as response:
                if response.status == 200 and registry_process_matches(pid, config):
                    return pid
        except (OSError, urllib.error.URLError):
            pass
        if not registry_process_matches(pid, config) or time.monotonic() >= deadline:
            if registry_process_matches(pid, config):
                os.kill(pid, signal.SIGTERM)
            raise RuntimeError(log.read_text())
        time.sleep(0.05)


def fetch_local(entry, layout):
    require(not layout.exists(), "output already exists; use an empty destination")
    repository, digest = reference(entry["local"])
    copy_image(f"docker://{repository}@sha256:{digest}", f"dir:{layout}",
               ["--src-no-creds", "--src-tls-verify=false"])
    return verify(layout, entry)


def prepare(entry, workdir, api_base="https://api.github.com",
            download_base="https://github.com"):
    """Create a fresh release-backed registry that remains live for Docker FROM."""
    require(not workdir.exists() and not workdir.is_symlink(), "workdir already exists")
    workdir.mkdir(mode=0o700)
    pid = None
    try:
        archive = workdir / entry["release"]["asset"]
        downloaded = download_release(entry, archive, api_base, download_base)
        layout = workdir / "layout"
        receipt = restore(archive, layout, entry)
        pid = start_registry(entry, workdir)
        repository, digest = reference(entry["local"])
        copy_image(f"dir:{layout}", f"docker://{repository}:sha256-{digest}",
                   ["--dest-no-creds", "--dest-tls-verify=false"])
        with tempfile.TemporaryDirectory(prefix="conary-ci-base-read-") as tmp:
            fetch_local(entry, Path(tmp) / "image")
        archive.unlink()
        shutil.rmtree(layout)
        (workdir / "registry.pid").write_text(f"{pid}\n")
        return {**receipt, **downloaded, "local_reference": resolve(entry["local"],
                {"image": entry}), "registry_pid": pid}
    except BaseException:
        if pid is not None and registry_process_matches(pid, workdir / "registry.json"):
            os.kill(pid, signal.SIGTERM)
        shutil.rmtree(workdir)
        raise


def cleanup(workdir):
    pid_file = workdir / "registry.pid"
    if not pid_file.exists():
        return
    pid = int(pid_file.read_text().strip())
    cmdline = Path(f"/proc/{pid}/cmdline")
    if cmdline.exists():
        command = cmdline.read_bytes().replace(b"\0", b" ")
        require(b"docker-registry serve" in command and str(workdir / "registry.json").encode() in command,
                "registry PID no longer belongs to this workdir")
        os.kill(pid, signal.SIGTERM)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                waited, _ = os.waitpid(pid, os.WNOHANG)
                if waited == pid:
                    break
            except ChildProcessError:
                if not cmdline.exists():
                    break
            time.sleep(0.05)
        require(not cmdline.exists(), "local registry did not stop")
    shutil.rmtree(workdir)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=CATALOG)
    commands = parser.add_subparsers(dest="command", required=True)
    resolve_parser = commands.add_parser("resolve", help="validate the CI pull reference")
    resolve_parser.add_argument("reference")
    resolve_parser.add_argument("--json", action="store_true")
    for name in ("verify", "restore", "stage", "prepare"):
        sub = commands.add_parser(name)
        sub.add_argument("--image", required=True)
        if name in ("verify", "restore", "stage"):
            sub.add_argument("--layout", type=Path, required=True)
        if name == "restore":
            sub.add_argument("--archive", type=Path, required=True)
        if name == "prepare":
            sub.add_argument("--workdir", type=Path, required=True)
        if name == "stage":
            sub.add_argument("--allow-loopback-http", action="store_true",
                             help="only for literal loopback registry fixtures")
    clean = commands.add_parser("cleanup", help="stop the job-local registry")
    clean.add_argument("--workdir", type=Path, required=True)
    args = parser.parse_args()
    try:
        entries = catalog(args.catalog)
        if args.command == "resolve":
            value = resolve(args.reference, entries)
            managed = any(reference(value) == reference(entry["local"])
                          for entry in entries.values())
            print(json.dumps({"reference": value, "release_backed": managed}) if args.json else value)
            return
        if args.command == "cleanup":
            cleanup(args.workdir)
            return
        entry = entries[args.image]
        if args.command == "verify":
            result = verify(args.layout, entry)
        elif args.command == "restore":
            result = restore(args.archive, args.layout, entry)
        elif args.command == "stage":
            result = stage(entry, args.layout, args.allow_loopback_http)
        else:
            result = prepare(entry, args.workdir)
        print(json.dumps(result, indent=2))
    except (ValueError, KeyError, OSError, RuntimeError, subprocess.SubprocessError,
            tarfile.TarError) as error:
        parser.exit(1, f"base image: {error}\n")


if __name__ == "__main__":
    main()
