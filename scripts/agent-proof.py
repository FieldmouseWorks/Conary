#!/usr/bin/env python3
"""Run one repo command and retain a revision-bound local proof receipt.

Usage: scripts/agent-proof.py [--run-id ID] -- COMMAND [ARG ...]

The candidate identity covers HEAD, the Git index, and every tracked or
nonignored untracked working-tree file. Ignored inputs and the environment are
outside that identity. The output lives under ignored target/agent-proof/.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import selectors
import signal
import stat
import subprocess
import sys
import time
from datetime import datetime, timezone


RUN_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}\Z")
STALE_EXIT = 125
INCOMPLETE_EXIT = 124
PIPE_EXIT_GRACE = 0.5
TERM_GRACE = 0.5
KILL_GRACE = 0.5
POLL_INTERVAL = 0.05
USAGE = "Usage: scripts/agent-proof.py [--run-id ID] -- COMMAND [ARG ...]"


class ProofError(Exception):
    pass


def git(root, *args):
    result = subprocess.run(
        ["git", *args], cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        check=False, env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"},
    )
    if result.returncode != 0:
        raise ProofError(f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout


def add_field(digest, value):
    digest.update(len(value).to_bytes(8, "big"))
    digest.update(value)


def file_identity(root, name, digest, metadata):
    path = root / os.fsdecode(name)
    add_field(digest, name)
    add_field(metadata, name)

    def missing():
        add_field(digest, b"missing")
        add_field(metadata, b"missing")

    parent = root
    for component in path.relative_to(root).parts[:-1]:
        parent /= component
        try:
            mode = parent.lstat().st_mode
        except FileNotFoundError:
            missing()
            return
        if stat.S_ISLNK(mode):
            raise ProofError(f"source parent is not a plain directory: {os.fsdecode(name)}")
        if not stat.S_ISDIR(mode):
            missing()
            return
    try:
        before = path.lstat()
    except FileNotFoundError:
        missing()
        return

    if stat.S_ISLNK(before.st_mode):
        add_field(digest, b"symlink")
        add_field(digest, os.fsencode(os.readlink(path)))
        after = path.lstat()
    elif stat.S_ISREG(before.st_mode):
        add_field(digest, b"file")
        add_field(digest, b"executable" if before.st_mode & 0o111 else b"plain")
        content = hashlib.sha256()
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        with os.fdopen(os.open(path, flags), "rb") as source:
            opened = os.fstat(source.fileno())
            if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
                raise ProofError(f"source changed while opening: {os.fsdecode(name)}")
            while chunk := source.read(1024 * 1024):
                content.update(chunk)
            closed = os.fstat(source.fileno())
        add_field(digest, content.digest())
        after = path.lstat()
        if (opened.st_dev, opened.st_ino, opened.st_size, opened.st_mtime_ns, opened.st_ctime_ns) != (
            closed.st_dev, closed.st_ino, closed.st_size, closed.st_mtime_ns, closed.st_ctime_ns
        ):
            raise ProofError(f"source changed while reading: {os.fsdecode(name)}")
    else:
        raise ProofError(f"unsupported source type: {os.fsdecode(name)}")

    if (before.st_dev, before.st_ino, before.st_mode, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
        after.st_dev, after.st_ino, after.st_mode, after.st_size, after.st_mtime_ns, after.st_ctime_ns
    ):
        raise ProofError(f"source changed while reading: {os.fsdecode(name)}")
    add_field(metadata, repr((before.st_dev, before.st_ino, before.st_mode, before.st_size, before.st_mtime_ns, before.st_ctime_ns)).encode())


def snapshot(root):
    head = git(root, "rev-parse", "HEAD").strip().decode("ascii")
    head_tree = git(root, "rev-parse", "HEAD^{tree}").strip().decode("ascii")
    index = git(root, "ls-files", "--stage", "-z")
    index_flags = git(root, "ls-files", "-v", "-z")
    tracked = set(filter(None, git(root, "ls-files", "--cached", "-z").split(b"\0")))
    untracked = set(filter(None, git(root, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0")))
    status = git(root, "status", "--porcelain=v1", "-z", "--untracked-files=all")
    digest = hashlib.sha256()
    metadata = hashlib.sha256()
    index_digest = hashlib.sha256()
    add_field(index_digest, index)
    add_field(index_digest, index_flags)
    for name in sorted(tracked | untracked):
        file_identity(root, name, digest, metadata)
    return {
        "head": head,
        "head_tree": head_tree,
        "index_sha256": index_digest.hexdigest(),
        "source_sha256": digest.hexdigest(),
        "source_metadata_sha256": metadata.hexdigest(),
        "tracked_paths": len(tracked),
        "untracked_paths": len(untracked),
        "state": "dirty" if status else "clean",
        "scope": "tracked and nonignored untracked working-tree paths; ignored files, external tools, and environment excluded",
    }


def stable_snapshot(root):
    first = snapshot(root)
    if snapshot(root) != first:
        raise ProofError("candidate changed while taking its identity")
    return first


def open_directory(parent_fd, name):
    try:
        os.mkdir(name, mode=0o700, dir_fd=parent_fd)
    except FileExistsError:
        pass
    try:
        return os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent_fd)
    except OSError as error:
        raise ProofError(f"evidence path is not a plain directory: {name}") from error


def ignored_output(root, run_id):
    result = subprocess.run(
        ["git", "check-ignore", "-q", "--", f"target/agent-proof/{run_id}/receipt.json"],
        cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=False,
    )
    if result.returncode != 0:
        raise ProofError("target/agent-proof must be ignored by Git before proof runs")


def evidence_directory(root, run_id):
    ignored_output(root, run_id)
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        target_fd = open_directory(root_fd, "target")
        try:
            proof_fd = open_directory(target_fd, "agent-proof")
            try:
                try:
                    os.mkdir(run_id, mode=0o700, dir_fd=proof_fd)
                except FileExistsError as error:
                    raise ProofError(f"evidence run already exists: {run_id}") from error
                return os.open(run_id, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=proof_fd)
            finally:
                os.close(proof_fd)
        finally:
            os.close(target_fd)
    finally:
        os.close(root_fd)


def exclusive_file(directory_fd, name):
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
    return os.fdopen(os.open(name, flags, 0o600, dir_fd=directory_fd), "wb")


def verify_log(directory_fd, name, opened, expected_hash, expected_size):
    try:
        path_before = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
        if not stat.S_ISREG(path_before.st_mode):
            return False, f"{name} is not a regular file"
        flags = os.O_RDONLY | os.O_NOFOLLOW
        with os.fdopen(os.open(name, flags, dir_fd=directory_fd), "rb") as source:
            before = os.fstat(source.fileno())
            digest = hashlib.sha256()
            size = 0
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
            after = os.fstat(source.fileno())
        path_after = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except OSError:
        return False, f"{name} is missing or unreadable"

    identity = lambda item: (item.st_dev, item.st_ino)
    state = lambda item: (item.st_mode, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
    if not stat.S_ISREG(before.st_mode) or any(identity(item) != identity(opened) for item in (path_before, before, after, path_after)):
        return False, f"{name} no longer names the opened regular file"
    if any(state(item) != state(before) for item in (path_before, after, path_after)):
        return False, f"{name} changed while verifying"
    if size != expected_size or digest.hexdigest() != expected_hash:
        return False, f"{name} differs from streamed output"
    return True, None


def run_child(argv, stdout_log, stderr_log):
    digests = [hashlib.sha256(), hashlib.sha256()]
    sizes = [0, 0]
    received = []
    child = None
    previous = {}

    def signal_group(signum):
        if child is None:
            return
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass

    def forward(signum, _frame):
        received.append(signum)
        signal_group(signum)

    for signum in (signal.SIGINT, signal.SIGTERM):
        previous[signum] = signal.signal(signum, forward)
    try:
        if received:
            return None, received[-1], digests, sizes, None, None
        try:
            child = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        except OSError as error:
            message = f"agent-proof: cannot start command: {error}\n".encode(errors="replace")
            stderr_log.write(message)
            stderr_log.flush()
            try:
                sys.stderr.buffer.write(message)
                sys.stderr.buffer.flush()
            except BrokenPipeError:
                pass
            digests[1].update(message)
            sizes[1] = len(message)
            return None, (received[-1] if received else None), digests, sizes, str(error), None

        if received:
            signal_group(received[-1])
        exited_at = None
        term_sent_at = None
        kill_sent_at = None
        incomplete = None
        with selectors.DefaultSelector() as selector:
            try:
                selector.register(child.stdout, selectors.EVENT_READ, (0, stdout_log, sys.stdout.buffer))
                selector.register(child.stderr, selectors.EVENT_READ, (1, stderr_log, sys.stderr.buffer))
                while True:
                    now = time.monotonic()
                    exited = child.poll() is not None
                    if exited and exited_at is None:
                        exited_at = now
                    if received and term_sent_at is None:
                        term_sent_at = now
                    if (exited_at is not None and selector.get_map() and not received
                            and incomplete is None and now - exited_at >= PIPE_EXIT_GRACE):
                        incomplete = "output pipes remained open after command exit; process group terminated"
                        signal_group(signal.SIGTERM)
                        term_sent_at = now
                    if (term_sent_at is not None and kill_sent_at is None
                            and now - term_sent_at >= TERM_GRACE
                            and (selector.get_map() or not exited)):
                        signal_group(signal.SIGKILL)
                        kill_sent_at = now
                    if kill_sent_at is not None and now - kill_sent_at >= KILL_GRACE:
                        if selector.get_map() or child.poll() is None:
                            incomplete = incomplete or "process group did not finish after termination; output closed"
                        for key in list(selector.get_map().values()):
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                        break
                    if not selector.get_map() and exited:
                        break
                    if selector.get_map():
                        events = selector.select(POLL_INTERVAL)
                    else:
                        try:
                            child.wait(timeout=POLL_INTERVAL)
                        except subprocess.TimeoutExpired:
                            pass
                        events = []
                    for key, _ in events:
                        data = os.read(key.fileobj.fileno(), 65536)
                        if not data:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                            continue
                        stream, log, display = key.data
                        log.write(data)
                        log.flush()
                        digests[stream].update(data)
                        sizes[stream] += len(data)
                        try:
                            display.write(data)
                            display.flush()
                        except BrokenPipeError:
                            pass
            except BaseException:
                signal_group(signal.SIGKILL)
                try:
                    child.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    pass
                raise
        return child.poll(), (received[-1] if received else None), digests, sizes, None, incomplete
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)


def write_receipt(directory_fd, receipt):
    payload = (json.dumps(receipt, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode()
    with exclusive_file(directory_fd, "receipt.json.partial") as output:
        output.write(payload)
        output.flush()
        os.fsync(output.fileno())
    os.link("receipt.json.partial", "receipt.json", src_dir_fd=directory_fd, dst_dir_fd=directory_fd, follow_symlinks=False)
    os.unlink("receipt.json.partial", dir_fd=directory_fd)
    os.fsync(directory_fd)


def arguments():
    args = sys.argv[1:]
    if args == ["-h"] or args == ["--help"]:
        print(USAGE)
        sys.exit(0)
    run_id = None
    if len(args) >= 2 and args[0] == "--run-id":
        run_id = args[1]
        args = args[2:]
        if not RUN_ID.fullmatch(run_id) or run_id in (".", ".."):
            raise ProofError("run ID must be a safe 1-64 character name")
    if not args or args[0] != "--" or len(args) < 2:
        raise ProofError(USAGE)
    return run_id or datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ-") + secrets.token_hex(6), args[1:]


def main():
    run_id, argv = arguments()
    try:
        root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").strip().decode()).resolve()
        initial = stable_snapshot(root)
        directory_fd = evidence_directory(root, run_id)
    except (OSError, UnicodeError, ProofError) as error:
        raise ProofError(str(error)) from error

    locator = f"target/agent-proof/{run_id}"
    started = datetime.now(timezone.utc)
    start_clock = time.monotonic()
    try:
        with exclusive_file(directory_fd, "stdout.log") as stdout_log, exclusive_file(directory_fd, "stderr.log") as stderr_log:
            opened_logs = (os.fstat(stdout_log.fileno()), os.fstat(stderr_log.fileno()))
            command_exit, received_signal, digests, sizes, launch_error, incomplete = run_child(argv, stdout_log, stderr_log)
            for log in (stdout_log, stderr_log):
                log.flush()
                os.fsync(log.fileno())
            final = None
            snapshot_error = None
            try:
                final = stable_snapshot(root)
            except (OSError, UnicodeError, ProofError) as error:
                snapshot_error = str(error)
            log_checks = [
                verify_log(directory_fd, name, opened, digest.hexdigest(), size)
                for name, opened, digest, size in zip(
                    ("stdout.log", "stderr.log"), opened_logs, digests, sizes
                )
            ]
        evidence_error = "; ".join(reason for verified, reason in log_checks if not verified)
        stale = final != initial or snapshot_error is not None
        child_signal = -command_exit if command_exit is not None and command_exit < 0 else None
        if received_signal or child_signal:
            status = "interrupted"
            result = 128 + (received_signal or child_signal)
        elif launch_error:
            status = "launch_error"
            result = 127
        elif evidence_error:
            status = "evidence_error"
            result = command_exit if command_exit else INCOMPLETE_EXIT
        elif incomplete:
            status = "incomplete"
            result = command_exit if command_exit else INCOMPLETE_EXIT
        elif stale:
            status = "stale"
            result = command_exit if command_exit else STALE_EXIT
        elif command_exit:
            status = "failed"
            result = command_exit
        else:
            status = "passed"
            result = 0

        ended = datetime.now(timezone.utc)
        receipt = {
            "schema_version": 1,
            "run_id": run_id,
            "argv": argv,
            "cwd": os.path.relpath(Path.cwd(), root),
            "candidate_before": initial,
            "candidate_after": final,
            "started_at": started.isoformat(),
            "ended_at": ended.isoformat(),
            "elapsed_seconds": round(time.monotonic() - start_clock, 6),
            "command_exit_code": command_exit if command_exit is not None and command_exit >= 0 else None,
            "command_signal": child_signal,
            "wrapper_signal": received_signal,
            "exit_code": result,
            "status": status,
            "output_complete": not (received_signal or child_signal or launch_error or incomplete or evidence_error),
            "stdout": {"path": f"{locator}/stdout.log", "sha256": digests[0].hexdigest(), "bytes": sizes[0], "verified": log_checks[0][0]},
            "stderr": {"path": f"{locator}/stderr.log", "sha256": digests[1].hexdigest(), "bytes": sizes[1], "verified": log_checks[1][0]},
        }
        if snapshot_error:
            receipt["snapshot_error"] = snapshot_error
        if launch_error:
            receipt["launch_error"] = launch_error
        if incomplete:
            receipt["incomplete_reason"] = incomplete
        if evidence_error:
            receipt["evidence_error"] = evidence_error
        write_receipt(directory_fd, receipt)
        print(f"agent-proof: {status}; receipt: {locator}/receipt.json", file=sys.stderr)
        return result
    finally:
        os.close(directory_fd)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ProofError as error:
        print(f"agent-proof: {error}", file=sys.stderr)
        sys.exit(2)
