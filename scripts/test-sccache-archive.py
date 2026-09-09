#!/usr/bin/env python3
"""Hermetic stdlib unittest coverage for the pinned sccache archive helper.

Exercises .github/actions/prepare-sccache/prepare.py through its real
prepare() entrypoint using fixture tar.gz archives and fake downloaders.
No network, no real release archive, and no fixture script execution.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import os
import shutil
import sys
import tarfile
import tempfile
import unittest
import uuid
from pathlib import Path

sys.dont_write_bytecode = True

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / ".github" / "actions" / "prepare-sccache" / "prepare.py"
SPEC = importlib.util.spec_from_file_location("prepare_sccache", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
PREPARE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PREPARE
SPEC.loader.exec_module(PREPARE)

FIXTURE_BINARY = b"fixture sccache payload for hermetic tests\n"
SENTINEL = b"sentinel-must-survive"


def write_archive(path, entries):
    """Write a fixture tar.gz and return its SHA-256 digest."""
    with tarfile.open(path, "w:gz") as archive:
        for entry in entries:
            info = tarfile.TarInfo(entry["name"])
            info.type = entry.get("type", tarfile.REGTYPE)
            info.mode = entry.get("mode", 0o644)
            if info.type in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                info.linkname = entry.get("linkname", "")
                info.size = 0
                archive.addfile(info)
            elif info.type == tarfile.REGTYPE:
                data = entry.get("data", b"")
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
            else:
                archive.addfile(info)
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


class PrepareSccacheArchiveTests(unittest.TestCase):
    maxDiff = None

    def setUp(self):
        self.root = tempfile.mkdtemp(prefix="sccache-test-")
        self.addCleanup(shutil.rmtree, self.root, True)
        self.runner_temp = os.path.join(self.root, "runner")
        self.tool_cache = os.path.join(self.root, "tool")
        self.fixtures = os.path.join(self.root, "fixtures")
        for path in (self.runner_temp, self.tool_cache, self.fixtures):
            os.makedirs(path)
        self.pin = PREPARE.load_pin()

    # -- helpers ---------------------------------------------------------

    def regular_member(self, data=FIXTURE_BINARY):
        return {"name": self.pin["archive_member"], "data": data}

    def fixture_archive(self, entries=None):
        path = os.path.join(self.fixtures, f"fixture-{uuid.uuid4().hex}.tar.gz")
        digest = write_archive(path, entries if entries is not None
                               else [self.regular_member()])
        return path, digest

    def pin_for(self, digest):
        pin = dict(self.pin)
        pin["sha256"] = digest
        return pin

    def place_archive(self, pin, source):
        destination = PREPARE.archive_cache_path(self.runner_temp, pin)
        os.makedirs(os.path.dirname(destination), exist_ok=True)
        shutil.copyfile(source, destination)
        return destination

    def valid_archive_pin(self):
        source, digest = self.fixture_archive()
        pin = self.pin_for(digest)
        self.place_archive(pin, source)
        return pin, source

    def copy_download(self, source, calls):
        def download(url, destination):
            calls.append((url, destination))
            shutil.copyfile(source, destination)
        return download

    def fail_download(self, url, destination):
        raise AssertionError(f"download must not be called: {url}")

    def write_sentinel(self, path, content=SENTINEL):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        Path(path).write_bytes(content)
        return (path, content)

    def assert_sentinel(self, sentinel):
        path, content = sentinel
        self.assertTrue(os.path.isfile(path), path)
        self.assertEqual(Path(path).read_bytes(), content, path)

    def tool_cache_entries(self):
        found = []
        for base, dirs, files in os.walk(self.tool_cache, followlinks=False):
            for name in dirs + files:
                found.append(os.path.relpath(os.path.join(base, name), self.tool_cache))
        return sorted(found)

    def expected_tool_cache_entries(self, pin):
        return [
            "sccache",
            os.path.join("sccache", pin["version"]),
            os.path.join("sccache", pin["version"], "x64"),
            os.path.join("sccache", pin["version"], "x64.complete"),
            os.path.join("sccache", pin["version"], "x64", "sccache"),
        ]

    def assert_no_verify_leftovers(self):
        self.assertFalse(
            [name for name in os.listdir(self.runner_temp)
             if name.startswith("sccache-verify-")],
            "prepare left a verification work directory behind")
        cache_dir = os.path.join(self.runner_temp, PREPARE.CACHE_SUBDIR)
        self.assertEqual(os.listdir(self.runner_temp), [PREPARE.CACHE_SUBDIR]
                         if os.path.isdir(cache_dir) else [])

    def assert_tool_cache_untouched(self, sentinel):
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(self.tool_cache), [os.path.basename(sentinel[0])])

    # -- pin contract ----------------------------------------------------

    def test_pin_contract_selects_0_16_0_linux_x64(self):
        self.assertEqual(self.pin["version"], "0.16.0")
        self.assertEqual(self.pin["target"], "x86_64-unknown-linux-musl")
        self.assertEqual(
            self.pin["url"],
            "https://github.com/mozilla/sccache/releases/download/v0.16.0/"
            "sccache-v0.16.0-x86_64-unknown-linux-musl.tar.gz")
        self.assertEqual(self.pin["archive_member"],
                         "sccache-v0.16.0-x86_64-unknown-linux-musl/sccache")
        self.assertEqual(PREPARE.tool_cache_dir(self.tool_cache, self.pin),
                         os.path.join(self.tool_cache, "sccache", "0.16.0", "x64"))
        self.assertEqual(PREPARE.cache_key(self.pin),
                         "conary-sccache-x86_64-unknown-linux-musl-v0.16.0-"
                         + self.pin["sha256"])


    def test_cold_download_runs_once_and_installs_binary_with_sibling_marker(self):
        source, digest = self.fixture_archive()
        pin = self.pin_for(digest)
        calls = []
        result = PREPARE.prepare(
            pin, runner_temp=self.runner_temp, tool_cache=self.tool_cache,
            cache_hit=False, download=self.copy_download(source, calls))

        self.assertEqual(len(calls), 1, "cold prepare must download exactly once")
        self.assertEqual(calls[0][0], pin["url"])
        archive = PREPARE.archive_cache_path(self.runner_temp, pin)
        self.assertNotEqual(calls[0][1], archive,
                            "download must stage outside the published cache path")
        self.assertTrue(os.path.dirname(calls[0][1]).startswith(
            os.path.dirname(archive) + os.sep))
        self.assertEqual(result["archive_path"], archive)
        self.assertEqual(result["cache_key"], PREPARE.cache_key(pin))
        self.assertEqual(Path(archive).read_bytes(), Path(source).read_bytes())

        version_dir = os.path.join(self.tool_cache, "sccache", pin["version"])
        target_dir = os.path.join(version_dir, "x64")
        marker = f"{target_dir}.complete"
        self.assertEqual(result["tool_cache_dir"], target_dir)

        binary = os.path.join(target_dir, "sccache")
        self.assertEqual(Path(binary).read_bytes(), FIXTURE_BINARY)
        self.assertEqual(os.stat(binary).st_mode & 0o777, 0o755)
        self.assertEqual(os.listdir(target_dir), ["sccache"],
                         "verified binary must be the only entry in the arch dir")
        self.assertTrue(os.path.isfile(marker))
        self.assertFalse(os.path.islink(marker))
        self.assertEqual(os.path.getsize(marker), 0, "marker must be empty")
        self.assertEqual(os.path.dirname(marker), version_dir,
                         "marker must be a sibling of x64, never inside PATH")
        self.assertEqual(sorted(os.listdir(version_dir)), ["x64", "x64.complete"])
        self.assertEqual(self.tool_cache_entries(),
                         self.expected_tool_cache_entries(pin))
        self.assert_no_verify_leftovers()


    def test_cache_hit_is_offline_and_never_downloads(self):
        source, digest = self.fixture_archive()
        pin = self.pin_for(digest)
        calls = []
        PREPARE.prepare(pin, runner_temp=self.runner_temp,
                        tool_cache=self.tool_cache, cache_hit=False,
                        download=self.copy_download(source, calls))
        self.assertEqual(len(calls), 1)

        shutil.rmtree(self.tool_cache)
        os.makedirs(self.tool_cache)
        result = PREPARE.prepare(
            pin, runner_temp=self.runner_temp, tool_cache=self.tool_cache,
            cache_hit=True, download=self.fail_download)

        self.assertEqual(len(calls), 1, "cache hit must not download again")
        binary = os.path.join(result["tool_cache_dir"], "sccache")
        self.assertEqual(Path(binary).read_bytes(), FIXTURE_BINARY)
        self.assertEqual(os.path.getsize(f"{result['tool_cache_dir']}.complete"), 0)
        self.assertEqual(os.listdir(result["tool_cache_dir"]), ["sccache"])


    def test_cache_hit_missing_archive_fails_before_install_without_redownload(self):
        pin = self.pin_for("a" * 64)
        sentinel = self.write_sentinel(os.path.join(self.tool_cache, "sentinel.bin"))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("not a regular file", str(caught.exception))
        self.assert_tool_cache_untouched(sentinel)
        self.assertFalse(os.path.lexists(
            PREPARE.tool_cache_dir(self.tool_cache, pin)))
        self.assertFalse(os.path.lexists(
            PREPARE.tool_cache_dir(self.tool_cache, pin) + ".complete"))
        self.assert_no_verify_leftovers()

    def test_cache_hit_corrupt_archive_fails_before_extraction_without_redownload(self):
        pin = self.pin_for("b" * 64)
        archive = PREPARE.archive_cache_path(self.runner_temp, pin)
        os.makedirs(os.path.dirname(archive), exist_ok=True)
        Path(archive).write_bytes(b"this is not a gzip tar archive")
        sentinel = self.write_sentinel(os.path.join(self.tool_cache, "sentinel.bin"))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("digest mismatch", str(caught.exception))
        self.assert_tool_cache_untouched(sentinel)
        self.assertFalse(os.path.lexists(
            PREPARE.tool_cache_dir(self.tool_cache, pin) + ".complete"))
        self.assert_no_verify_leftovers()
        self.assertEqual(os.listdir(os.path.dirname(archive)),
                         [os.path.basename(archive)],
                         "the corrupt restored archive must stay as restored")

    def test_cache_hit_valid_archive_with_wrong_digest_fails_before_extraction(self):
        source, _digest = self.fixture_archive()
        pin = self.pin_for("c" * 64)
        self.place_archive(pin, source)
        sentinel = self.write_sentinel(os.path.join(self.tool_cache, "sentinel.bin"))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("digest mismatch", str(caught.exception))
        self.assert_tool_cache_untouched(sentinel)
        self.assert_no_verify_leftovers()


    def test_extra_archive_entries_are_never_written(self):
        token = uuid.uuid4().hex
        prefix = f"sccache-v{self.pin['version']}-{self.pin['target']}"
        escape = f"escape-{token}.txt"
        escape_parent = f"escape-parent-{token}.txt"
        absolute = os.path.join(self.root, f"abs-escape-{token}.txt")
        bash_name = f"bash-{token}"
        evil = f"evil-{token}"
        source, digest = self.fixture_archive([
            {"name": self.pin["archive_member"], "data": FIXTURE_BINARY},
            {"name": f"../{escape}", "data": b"escape"},
            {"name": f"../../{escape_parent}", "data": b"escape-parent"},
            {"name": absolute, "data": b"absolute"},
            {"name": bash_name, "data": b"#!/bin/sh\n"},
            {"name": f"{prefix}/{bash_name}", "data": b"#!/bin/sh\n"},
            {"name": f"{prefix}/{evil}", "data": b"evil"},
            {"name": f"{prefix}/evil-link", "type": tarfile.SYMTYPE,
             "linkname": "/etc/passwd"},
        ])
        pin = self.pin_for(digest)
        result = PREPARE.prepare(
            pin, runner_temp=self.runner_temp, tool_cache=self.tool_cache,
            cache_hit=False, download=self.copy_download(source, []))

        binary = os.path.join(result["tool_cache_dir"], "sccache")
        self.assertEqual(Path(binary).read_bytes(), FIXTURE_BINARY)
        for path in (os.path.join(self.runner_temp, escape),
                     os.path.join(os.path.dirname(self.runner_temp), escape_parent),
                     absolute,
                     os.path.join(self.tool_cache, bash_name),
                     os.path.join(self.tool_cache, evil),
                     os.path.join(self.tool_cache, "sccache", pin["version"], evil),
                     os.path.join(result["tool_cache_dir"], evil),
                     os.path.join(result["tool_cache_dir"], bash_name),
                     os.path.join(result["tool_cache_dir"], "evil-link")):
            self.assertFalse(os.path.lexists(path),
                             f"unexpected archive entry was written: {path}")
        self.assertEqual(self.tool_cache_entries(),
                         self.expected_tool_cache_entries(pin))
        self.assert_no_verify_leftovers()

    def test_member_symlink_hardlink_duplicate_and_missing_reject(self):
        member = self.pin["archive_member"]
        cases = {
            "symlink": [{"name": member, "type": tarfile.SYMTYPE,
                         "linkname": "sccache"}],
            "hardlink": [{"name": member, "type": tarfile.LNKTYPE,
                          "linkname": member}],
            "duplicate": [{"name": member, "data": FIXTURE_BINARY},
                          {"name": member, "data": FIXTURE_BINARY}],
            "missing": [{"name": member + ".other", "data": FIXTURE_BINARY}],
        }
        for label, entries in cases.items():
            with self.subTest(label=label):
                source, digest = self.fixture_archive(entries)
                pin = self.pin_for(digest)
                sentinel = self.write_sentinel(
                    os.path.join(self.tool_cache, "sentinel.bin"))
                expected = "not a regular file" if label in ("symlink", "hardlink") else "expected exactly one"
                with self.assertRaisesRegex(PREPARE.PrepareError, expected):
                    PREPARE.prepare(
                        pin, runner_temp=self.runner_temp,
                        tool_cache=self.tool_cache, cache_hit=False,
                        download=self.copy_download(source, []))
                self.assert_tool_cache_untouched(sentinel)
                self.assertFalse(os.path.lexists(
                    PREPARE.tool_cache_dir(self.tool_cache, pin)))
                self.assertFalse(os.path.lexists(
                    PREPARE.tool_cache_dir(self.tool_cache, pin) + ".complete"))
                self.assert_no_verify_leftovers()
                shutil.rmtree(self.tool_cache)
                os.makedirs(self.tool_cache)
                os.unlink(PREPARE.archive_cache_path(self.runner_temp, pin))


    def test_symlinked_runner_temp_rejects_without_touching_sentinel(self):
        real = os.path.join(self.root, "real-runner")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        link = os.path.join(self.root, "runner-link")
        os.symlink(real, link)
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(self.pin_for("d" * 64), runner_temp=link,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])

    def test_symlinked_cache_directory_rejects_without_touching_sentinel(self):
        real = os.path.join(self.root, "cache-target")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        os.symlink(real, os.path.join(self.runner_temp, PREPARE.CACHE_SUBDIR))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(self.pin_for("d" * 64), runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=False,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])
        self.assertEqual(os.listdir(self.tool_cache), [])

    def test_symlinked_cached_archive_rejects_without_touching_sentinel(self):
        source, digest = self.fixture_archive()
        pin = self.pin_for(digest)
        real = os.path.join(self.root, "real-archive.tar.gz")
        shutil.copyfile(source, real)
        archive = PREPARE.archive_cache_path(self.runner_temp, pin)
        os.makedirs(os.path.dirname(archive), exist_ok=True)
        os.symlink(real, archive)
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("not a regular file", str(caught.exception))
        self.assertTrue(os.path.islink(archive))
        self.assertEqual(Path(real).read_bytes(), Path(source).read_bytes())
        self.assertEqual(os.listdir(self.tool_cache), [])

    def test_symlinked_tool_cache_rejects_without_touching_sentinel(self):
        pin, _source = self.valid_archive_pin()
        real = os.path.join(self.root, "real-tool")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        link = os.path.join(self.root, "tool-link")
        os.symlink(real, link)
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp, tool_cache=link,
                            cache_hit=True, download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])

    def test_symlinked_tool_cache_sccache_rejects_without_touching_sentinel(self):
        pin, _source = self.valid_archive_pin()
        real = os.path.join(self.root, "real-sccache")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        os.symlink(real, os.path.join(self.tool_cache, "sccache"))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])

    def test_symlinked_tool_cache_version_rejects_without_touching_sentinel(self):
        pin, _source = self.valid_archive_pin()
        os.makedirs(os.path.join(self.tool_cache, "sccache"))
        real = os.path.join(self.root, "real-version")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        os.symlink(real, os.path.join(self.tool_cache, "sccache", pin["version"]))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])

    def test_symlinked_tool_cache_arch_rejects_without_touching_sentinel(self):
        pin, _source = self.valid_archive_pin()
        version_dir = os.path.join(self.tool_cache, "sccache", pin["version"])
        os.makedirs(version_dir)
        real = os.path.join(self.root, "real-arch")
        os.makedirs(real)
        sentinel = self.write_sentinel(os.path.join(real, "sentinel.bin"))
        os.symlink(real, os.path.join(version_dir, "x64"))
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assert_sentinel(sentinel)
        self.assertEqual(os.listdir(real), [os.path.basename(sentinel[0])])

    def test_symlinked_tool_cache_marker_rejects_without_touching_sentinel(self):
        pin, _source = self.valid_archive_pin()
        version_dir = os.path.join(self.tool_cache, "sccache", pin["version"])
        os.makedirs(version_dir)
        target = os.path.join(self.root, "marker-target")
        sentinel = self.write_sentinel(target)
        marker = os.path.join(version_dir, "x64.complete")
        os.symlink(target, marker)
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=True,
                            download=self.fail_download)
        self.assertIn("symlink", str(caught.exception))
        self.assertTrue(os.path.islink(marker))
        self.assert_sentinel(sentinel)


    def test_stale_tool_cache_directory_is_replaced_completely(self):
        pin, _source = self.valid_archive_pin()
        target_dir = PREPARE.tool_cache_dir(self.tool_cache, pin)
        version_dir = os.path.dirname(target_dir)
        os.makedirs(target_dir)
        Path(os.path.join(target_dir, "sccache")).write_bytes(b"stale-binary")
        unwanted = os.path.join(target_dir, "unwanted-executable")
        Path(unwanted).write_bytes(b"#!/bin/sh\necho unwanted\n")
        os.chmod(unwanted, 0o755)
        marker = f"{target_dir}.complete"
        Path(marker).write_bytes(b"stale-marker")

        result = PREPARE.prepare(pin, runner_temp=self.runner_temp,
                                 tool_cache=self.tool_cache, cache_hit=True,
                                 download=self.fail_download)

        self.assertEqual(result["tool_cache_dir"], target_dir)
        self.assertEqual(os.listdir(target_dir), ["sccache"],
                         "stale PATH directory must be replaced completely")
        binary = os.path.join(target_dir, "sccache")
        self.assertEqual(Path(binary).read_bytes(), FIXTURE_BINARY)
        self.assertEqual(os.stat(binary).st_mode & 0o777, 0o755)
        self.assertFalse(os.path.lexists(unwanted))
        self.assertEqual(os.path.getsize(marker), 0, "marker must be rewritten empty")
        self.assertEqual(sorted(os.listdir(version_dir)), ["x64", "x64.complete"])
        self.assertEqual(self.tool_cache_entries(),
                         self.expected_tool_cache_entries(pin))


    def test_download_checksum_mismatch_publishes_nothing(self):
        source, _digest = self.fixture_archive()
        pin = self.pin_for("0" * 64)
        sentinel = self.write_sentinel(os.path.join(self.tool_cache, "sentinel.bin"))
        calls = []
        with self.assertRaises(PREPARE.PrepareError) as caught:
            PREPARE.prepare(pin, runner_temp=self.runner_temp,
                            tool_cache=self.tool_cache, cache_hit=False,
                            download=self.copy_download(source, calls))
        self.assertIn("digest mismatch", str(caught.exception))
        self.assertEqual(len(calls), 1)
        archive = PREPARE.archive_cache_path(self.runner_temp, pin)
        self.assertFalse(os.path.lexists(archive),
                         "mismatched download must not publish the cache archive")
        self.assertEqual(os.listdir(os.path.dirname(archive)), [],
                         "download staging directory must be cleaned up")
        self.assert_tool_cache_untouched(sentinel)
        self.assertFalse(os.path.lexists(
            PREPARE.tool_cache_dir(self.tool_cache, pin)))
        self.assertFalse(os.path.lexists(
            PREPARE.tool_cache_dir(self.tool_cache, pin) + ".complete"),
            "checksum mismatch must not publish the tool-cache marker")


if __name__ == "__main__":
    unittest.main()
