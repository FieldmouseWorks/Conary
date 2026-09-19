#!/usr/bin/env python3
"""Retain reviewed CI image bytes; registry clients do transport, SHA-256 owns identity."""

import argparse
import hashlib
import ipaddress
import json
from pathlib import Path
import re
import stat
import subprocess
import tempfile
from urllib.parse import urlsplit


CATALOG = Path(__file__).with_name("ci-base-images.json")
HEX = re.compile(r"[0-9a-f]{64}")
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
            and type(data["version"]) is int and data["version"] == 1,
            "unsupported base-image catalog")
    require(isinstance(data["images"], dict) and data["images"], "empty image catalog")
    seen = set()
    for entry in data["images"].values():
        require(isinstance(entry, dict) and set(entry) == {"source", "retained", "platform"},
                "invalid image entry")
        source, digest = reference(entry["source"])
        retained, retained_digest = reference(entry["retained"])
        require(digest == retained_digest and source != retained,
                "retention must preserve the source digest at a distinct repository")
        require(entry["platform"] == "linux/amd64", "only reviewed linux/amd64 manifests are supported")
        require(entry["retained"] not in seen, "duplicate retained reference")
        seen.add(entry["retained"])
    return data["images"]


def resolve(value, entries):
    repository, digest = reference(value)
    for entry in entries.values():
        require((repository, digest) != reference(entry["source"]),
                "retained image must be consumed through its retained reference")
        if (repository, digest) == reference(entry["retained"]):
            return f"{repository}@sha256:{digest}"
    require(not repository.startswith("ghcr.io/fieldmouseworks/conary-ci-base-"),
            "retained image is not registered in the reviewed catalog")
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
        require(not descriptor.get("urls"), "external layer URLs are not retained authority")
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
                    source, destination], check=True, timeout=600)


def fetch(entry, layout, origin=False, allow_http=False):
    require(not layout.exists(), "output already exists; use an empty destination")
    value = entry["source" if origin else "retained"]
    repository, digest = reference(value)
    # Do not consult ambient registry credentials: CI/public read must be anonymous.
    flags = ["--src-no-creds", *transport_flags(value, "src", allow_http)]
    copy_image(f"docker://{repository}@sha256:{digest}", f"dir:{layout}", flags)
    return verify(layout, entry)


def publish(entry, layout, auth, allow_http=False):
    receipt = verify(layout, entry)
    require(auth.is_file(), "publication requires an explicit registry auth file")
    repository, digest = reference(entry["retained"])
    # A permanent digest-derived tag keeps the exact manifest reachable for retention.
    flags = ["--dest-authfile", str(auth),
             *transport_flags(entry["retained"], "dest", allow_http)]
    copy_image(f"dir:{layout}", f"docker://{repository}:sha256-{digest}", flags)
    # Publication is not successful until an anonymous, fresh download verifies every byte.
    with tempfile.TemporaryDirectory(prefix="conary-retained-read-") as tmp:
        fetch(entry, Path(tmp) / "image", allow_http=allow_http)
    return {**receipt, "anonymous_read_verified": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", type=Path, default=CATALOG)
    commands = parser.add_subparsers(dest="command", required=True)
    resolve_parser = commands.add_parser("resolve", help="validate the CI pull reference")
    resolve_parser.add_argument("reference")
    for name in ("verify", "stage", "fetch", "publish"):
        sub = commands.add_parser(name)
        sub.add_argument("--image", required=True)
        sub.add_argument("--layout", type=Path, required=True)
        if name in ("stage", "fetch", "publish"):
            sub.add_argument("--allow-loopback-http", action="store_true",
                             help="only for literal loopback registry fixtures")
        if name == "publish":
            sub.add_argument("--authfile", type=Path, required=True)
    args = parser.parse_args()
    try:
        entries = catalog(args.catalog)
        if args.command == "resolve":
            print(resolve(args.reference, entries))
            return
        entry = entries[args.image]
        if args.command == "verify":
            result = verify(args.layout, entry)
        elif args.command == "publish":
            result = publish(entry, args.layout, args.authfile, args.allow_loopback_http)
        else:
            result = fetch(entry, args.layout, args.command == "stage", args.allow_loopback_http)
        print(json.dumps(result, indent=2))
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"base image: {error}\n")


if __name__ == "__main__":
    main()
