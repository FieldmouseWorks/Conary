#!/usr/bin/env python3
"""Export public, source-pinned Conary context cases; never call a provider."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "scripts/fixtures/context-selection-v1.json"


def encoded(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(",", ":")).encode()


def digest(value):
    return hashlib.sha256(value).hexdigest()


def source(revision, path):
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("source revision must be a full immutable commit")
    if Path(path).is_absolute() or ".." in Path(path).parts or "\n" in path:
        raise ValueError("invalid repository source path")
    return subprocess.check_output(["git", "show", f"{revision}:{path}"], cwd=ROOT).decode()


def chunk(identity, origin, text):
    return {"id": identity, "source": origin, "text": text, "sha256": digest(text.encode())}


def export(spec):
    if spec["version"] != 1:
        raise ValueError("unsupported corpus version")
    revision = spec["source_revision"]
    policy = source(revision, "AGENTS.md")
    subprocess.run(["git", "diff", "--quiet", revision, "--", "apps", "crates",
                    "Cargo.toml", "Cargo.lock", "third_party"], cwd=ROOT, check=True)
    manifest = {"version": 1, "source_revision": revision,
                "required": [chunk("project_policy", f"{revision}:AGENTS.md", policy)],
                "limits": spec["limits"], "cases": []}
    truth = {}
    with tempfile.TemporaryDirectory(prefix="conary-context-") as temporary:
        temporary = Path(temporary)
        router = temporary / "agent-context.sh"
        owner_map = temporary / "feature-ownership.md"
        router.write_text(source(revision, "scripts/agent-context.sh"))
        owner_map.write_text(source(revision, "docs/modules/feature-ownership.md"))
        for case in spec["cases"]:
            packet = subprocess.check_output(
                ["bash", str(router), "--path", case["route_path"], "--map", str(owner_map)],
                cwd=ROOT, text=True)
            if not packet.startswith("# Task Packet:"):
                raise ValueError(f"missing owner packet for {case['id']}")
            first = packet.split("## Read first\n", 1)[1].split("\n## ", 1)[0]
            route_order = [case["route_path"]] + re.findall(r"`([^`]+)`", first)
            chunks = []
            for item in case["chunks"]:
                text = source(revision, item["path"])
                lines = text.splitlines(keepends=True)
                if not 1 <= item["start"] <= item["end"] <= len(lines):
                    raise ValueError("invalid source range")
                excerpt = "".join(lines[item["start"] - 1:item["end"]])
                if digest(excerpt.encode()) != item["sha256"]:
                    raise ValueError("source excerpt hash mismatch")
                origin = f"{revision}:{item['path']}:{item['start']}-{item['end']}"
                chunks.append(chunk(item["id"], origin, excerpt))
            # Stronger than blindly taking the first router entry: always put
            # the task's concrete source path first, then the router's read-first
            # order, then unlisted neighbors by path and source line.
            def rank(item):
                try:
                    position = route_order.index(item["path"])
                except ValueError:
                    position = len(route_order)
                return position, item["path"], item["start"], item["id"]
            order = [item["id"] for item in sorted(case["chunks"], key=rank)]
            manifest["cases"].append({
                "id": case["id"], "split": case["split"], "task": case["task"],
                "mandatory": [chunk("owner_packet", f"{revision}:owner-packet:{case['route_path']}", packet)],
                "chunks": chunks, "baseline_order": order, "diagnoses": case["diagnoses"],
            })
            for proof in case["proof_sources"]:
                if f"fn {proof['function']}(" not in source(revision, proof["path"]):
                    raise ValueError("independent proof function missing")
                if (ROOT / proof["path"]).read_text() != source(revision, proof["path"]):
                    raise ValueError("working proof differs from pinned source")
            truth[case["id"]] = {"diagnosis": case["expected"], "essential": case["essential"],
                                  "proof": case["proof_commands"]}
    oracle = {"manifest_sha256": digest(encoded(manifest)), "cases": truth}
    return manifest, oracle


def check(spec, manifest, oracle):
    again = export(spec)
    if (manifest, oracle) != again:
        raise ValueError("export is not deterministic")
    changed = copy.deepcopy(spec)
    for case in changed["cases"]:
        case["expected"] = "insufficient"
        case["proof_commands"] = ["GRADER_ONLY_CANARY"]
    changed_manifest, changed_oracle = export(changed)
    if manifest != changed_manifest or oracle == changed_oracle:
        raise ValueError("grader data changed model-visible manifest")
    if b"GRADER_ONLY_CANARY" in encoded(changed_manifest):
        raise ValueError("grading label leaked")
    corrupt = copy.deepcopy(spec)
    corrupt["cases"][0]["chunks"][0]["sha256"] = "0" * 64
    try:
        export(corrupt)
    except ValueError as error:
        if str(error) != "source excerpt hash mismatch":
            raise
    else:
        raise ValueError("changed source hash was accepted")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not args.check and args.output is None:
        parser.error("use --check and/or --output")
    spec = json.loads(SPEC.read_text())
    manifest, oracle = export(spec)
    if args.check:
        check(spec, manifest, oracle)
    if args.output:
        args.output.mkdir(mode=0o700)
        (args.output / "manifest.json").write_bytes(encoded(manifest))
        (args.output / "oracle.json").write_bytes(encoded(oracle))
    print(json.dumps({"cases": len(manifest["cases"]), "manifest_sha256": oracle["manifest_sha256"],
                      "checked": args.check, "provider_calls": 0}))


if __name__ == "__main__":
    main()
