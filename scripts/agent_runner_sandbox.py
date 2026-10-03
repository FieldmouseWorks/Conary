# scripts/agent_runner_sandbox.py
"""Run a bounded Codex child with no host home or GitHub credentials.

The controller owns commits, remote writes, and result validation. The child
gets a writable worktree, read-only Git metadata, a private home, and one
model credential copied into that home. Its raw JSONL is never persisted.
"""

from __future__ import annotations

import json
import hashlib
import os
import re
import shutil
import signal
import stat
import subprocess
import tempfile
import threading
from dataclasses import dataclass
from pathlib import Path
from pathlib import PurePosixPath
from urllib.parse import urlsplit


MAX_PROMPT_BYTES = 256 * 1024
MAX_RESULT_BYTES = 256 * 1024
MAX_EVENT_BYTES = 1024 * 1024
EVENT_NAME = re.compile(r"^[a-z][a-z0-9_.-]{0,63}$")
MODEL_NAME = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9._-]{0,79}$")
REASONING_EFFORTS = frozenset({"minimal", "low", "medium", "high", "xhigh", "max", "ultra"})
HEAD_ID = re.compile(r"^[0-9a-f]{40}$")
RESULT_KEYS = frozenset({
    "schema_version", "run_id", "task_id", "status", "branch", "candidate_head",
    "candidate_tree", "evidence", "reason",
})
RESULT_SCHEMA = {
    "type": "object",
    "additionalProperties": False,
    "required": sorted(RESULT_KEYS),
    "properties": {
        "schema_version": {"type": "integer", "enum": [1]},
        "run_id": {"type": "string"},
        "task_id": {"type": "string"},
        "status": {"type": "string", "enum": ["ready", "blocked"]},
        "branch": {"type": ["string", "null"]},
        "candidate_head": {"type": ["string", "null"]},
        "candidate_tree": {"type": ["string", "null"]},
        "evidence": {"type": "array", "items": {"type": "string"}},
        "reason": {"type": ["string", "null"]},
    },
}


@dataclass(frozen=True)
class LaunchResult:
    ok: bool
    exit_code: int | None
    timed_out: bool
    model: str
    reasoning_effort: str
    session_id: str | None
    error: str | None
    trace_path: Path
    result_path: Path


@dataclass(frozen=True)
class ProofLaunchResult:
    ok: bool
    exit_code: int | None
    timed_out: bool
    error: str | None


def _git(worktree: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=worktree, check=False, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL, text=True, env={"PATH": "/usr/bin:/bin"},
    )
    if result.returncode:
        raise ValueError("worktree Git metadata is unavailable")
    return result.stdout.strip()


def _git_metadata(worktree: Path) -> Path:
    pointer = worktree / ".git"
    info = pointer.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 1024:
        raise ValueError("worktree Git pointer must be an owned regular file")
    if Path(_git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree:
        raise ValueError("worktree must be the Git top level")
    common = Path(_git(worktree, "rev-parse", "--path-format=absolute", "--git-common-dir")).resolve()
    if not common.is_dir() or common == worktree or worktree in common.parents:
        raise ValueError("invalid common Git directory")
    # A repository-local credential helper or token-bearing remote would undo
    # the private-home guarantee even though the common directory is read-only.
    git_dir = Path(_git(worktree, "rev-parse", "--path-format=absolute", "--git-dir")).resolve()
    if git_dir != common and common not in git_dir.parents:
        raise ValueError("worktree Git directory escapes common Git metadata")
    pointer_text = pointer.read_text(encoding="utf-8")
    if not pointer_text.startswith("gitdir: ") or Path(pointer_text[8:].strip()).resolve() != git_dir:
        raise ValueError("worktree Git pointer disagrees with Git metadata")
    for config in (common / "config", git_dir / "config.worktree"):
        if not config.is_file():
            continue
        listing = subprocess.run(
            ["git", "config", "--file", str(config), "--null", "--list"],
            check=False, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            env={"PATH": "/usr/bin:/bin"},
        )
        if listing.returncode:
            raise ValueError("invalid repository Git config")
        for record in listing.stdout.split(b"\0"):
            if not record:
                continue
            key, _, value = record.partition(b"\n")
            name = key.decode("utf-8", errors="replace").lower()
            if (name.startswith("credential.") or name.startswith("url.")
                    or name.startswith("http.") or name == "core.sshcommand"
                    or name.endswith(".pushurl") or name.startswith("include.")
                    or name.startswith("includeif.")):
                raise ValueError("repository Git config contains credential-capable settings")
            if name.startswith("remote.") and name.endswith(".url"):
                remote = value.decode("utf-8", errors="replace")
                parsed = urlsplit(remote)
                if parsed.password or (parsed.scheme in ("http", "https") and parsed.username):
                    raise ValueError("repository remote URL contains credentials")
    return common


def _copy_auth(source: Path, destination: Path) -> set[str]:
    fd = os.open(source, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o077 or info.st_size > MAX_RESULT_BYTES):
            raise ValueError("Codex auth file has unsafe ownership or permissions")
        with os.fdopen(fd, "rb", closefd=False) as handle:
            payload = handle.read(MAX_RESULT_BYTES + 1)
    finally:
        os.close(fd)
    if len(payload) > MAX_RESULT_BYTES:
        raise ValueError("Codex auth file is too large")

    def unique_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
        parsed: dict[str, object] = {}
        for key, value in pairs:
            if key in parsed:
                raise ValueError("Codex auth file has duplicate keys")
            parsed[key] = value
        return parsed

    try:
        auth = json.loads(payload, object_pairs_hook=unique_keys)
    except (ValueError, UnicodeDecodeError) as exc:
        raise ValueError("Codex auth file is invalid") from exc
    secrets: set[str] = set()

    def collect(value: object) -> None:
        if isinstance(value, str) and len(value) >= 16:
            secrets.add(value)
        elif isinstance(value, dict):
            for item in value.values():
                collect(item)
        elif isinstance(value, list):
            for item in value:
                collect(item)

    collect(auth)
    out_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(out_fd, "wb") as handle:
            handle.write(payload)
    finally:
        del payload
    return secrets


def _toolchain_mounts() -> tuple[list[str], str]:
    """Expose only the selected Rust toolchain and cached public crate sources."""
    mounts: list[str] = []
    paths = ["/usr/bin", "/bin"]
    try:
        result = subprocess.run(
            ["rustup", "which", "cargo"], check=False, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True,
        )
        if result.returncode == 0:
            cargo = Path(result.stdout.strip()).resolve()
            toolchain = cargo.parent.parent
            if cargo.is_file() and toolchain.is_dir() and toolchain.name:
                mounts += ["--ro-bind", str(toolchain), "/opt/rust-toolchain"]
                paths.insert(0, "/opt/rust-toolchain/bin")
    except OSError:
        pass
    cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
    registry = cargo_home / "registry"
    if registry.is_dir():
        mounts += ["--ro-bind", str(registry.resolve()), "/home/agent/.cargo/registry"]
    return mounts, ":".join(paths)


def _restricted_argv(
    *, worktree: Path, common_git: Path, private_home: Path,
    executable: Path, mounted_executable: str, bwrap_binary: Path,
    child_argv: list[str], read_only_worktree: bool,
    extra_mounts: list[str] | None = None,
) -> list[str]:
    toolchain_mounts, path_env = _toolchain_mounts()
    argv = [
        str(bwrap_binary), "--die-with-parent", "--new-session", "--unshare-user",
        "--unshare-pid", "--unshare-ipc", "--unshare-uts", "--disable-userns",
        "--cap-drop", "ALL", "--ro-bind", "/usr", "/usr",
        "--ro-bind", "/bin", "/bin", "--ro-bind", "/lib", "/lib",
        "--ro-bind", "/lib64", "/lib64", "--dir", "/etc",
    ]
    # Compiler aliases such as /usr/bin/cc resolve through /etc/alternatives.
    alternatives = Path("/etc/alternatives")
    if alternatives.is_dir():
        argv += ["--ro-bind", str(alternatives), str(alternatives)]
    for entry in ("ssl", "resolv.conf", "hosts", "nsswitch.conf", "passwd", "group", "localtime"):
        source = Path("/etc") / entry
        if source.exists():
            argv += ["--ro-bind", str(source), str(source)]
    argv += [
        "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
        "--dir", "/home", "--bind", str(private_home), "/home/agent",
        "--ro-bind" if read_only_worktree else "--bind", str(worktree), str(worktree),
        "--ro-bind", str(worktree / ".git"), str(worktree / ".git"),
        "--ro-bind", str(common_git), str(common_git),
        "--ro-bind", str(executable), mounted_executable,
        *(extra_mounts or []),
        *toolchain_mounts, "--clearenv",
    ]
    environment = {
        "HOME": "/home/agent", "CODEX_HOME": "/home/agent/.codex",
        "XDG_CONFIG_HOME": "/home/agent/.config", "XDG_CACHE_HOME": "/home/agent/.cache",
        "XDG_DATA_HOME": "/home/agent/.local/share", "CARGO_HOME": "/home/agent/.cargo",
        "CARGO_NET_OFFLINE": "true", "PATH": path_env, "TMPDIR": "/tmp",
        "GH_CONFIG_DIR": "/home/agent/.config/gh", "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_TERMINAL_PROMPT": "0",
        "GIT_SSH_COMMAND": "/bin/false", "GIT_ALLOW_PROTOCOL": "file",
        "LANG": "C.UTF-8",
    }
    for key, value in environment.items():
        argv += ["--setenv", key, value]
    argv += ["--chdir", str(worktree), "--", *child_argv]
    return argv


def _sandbox_argv(
    *, worktree: Path, common_git: Path, private_home: Path,
    codex_binary: Path, bwrap_binary: Path, model: str, effort: str,
    read_only_worktree: bool,
) -> tuple[list[str], list[str]]:
    companion = codex_binary.with_name("codex-code-mode-host")
    companion_mounts: list[str] = []
    if companion.is_file() and os.access(companion, os.X_OK):
        companion_mounts = ["--ro-bind", str(companion), "/opt/agent/codex-code-mode-host"]
    child_argv = [
        "/opt/agent/codex", "exec", "--json", "--ephemeral", "--ignore-user-config",
        "--model", model, "-c", f"model_reasoning_effort={effort}",
        "--dangerously-bypass-approvals-and-sandbox", "--output-schema",
        "/home/agent/result-schema.json", "--output-last-message",
        "/home/agent/result.json", "-C", str(worktree), "-",
    ]
    argv = _restricted_argv(
        worktree=worktree, common_git=common_git, private_home=private_home,
        executable=codex_binary, mounted_executable="/opt/agent/codex",
        bwrap_binary=bwrap_binary, child_argv=child_argv,
        read_only_worktree=read_only_worktree, extra_mounts=companion_mounts,
    )
    return argv, child_argv


def _exclusive_file(path: Path):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    return os.fdopen(fd, "w", encoding="utf-8")


def _stop_process_group(child: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def _kill_process_group(child: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _safe_result(final: object, secrets: set[str]) -> bool:
    if not isinstance(final, dict) or set(final) != RESULT_KEYS:
        return False
    if type(final["schema_version"]) is not int or final["schema_version"] != 1:
        return False
    if final["status"] not in ("ready", "blocked"):
        return False
    for name in ("run_id", "task_id"):
        value = final[name]
        if not isinstance(value, str) or not (1 <= len(value) <= 128):
            return False
    branch = final["branch"]
    if branch is not None and (not isinstance(branch, str) or not (1 <= len(branch) <= 256)):
        return False
    for name in ("candidate_head", "candidate_tree"):
        value = final[name]
        if value is not None and (not isinstance(value, str) or not HEAD_ID.fullmatch(value)):
            return False
    if final["status"] == "ready" and branch is None:
        return False
    reason = final["reason"]
    if reason is not None and (not isinstance(reason, str) or len(reason) > 500):
        return False
    evidence = final["evidence"]
    if not isinstance(evidence, list) or len(evidence) > 256:
        return False
    for entry in evidence:
        if (not isinstance(entry, str) or not entry or len(entry) > 1024
                or PurePosixPath(entry).is_absolute() or ".." in PurePosixPath(entry).parts):
            return False

    def strings(value: object):
        if isinstance(value, str):
            yield value
        elif isinstance(value, dict):
            for item in value.values():
                yield from strings(item)
        elif isinstance(value, list):
            for item in value:
                yield from strings(item)

    return not any(secret in value for value in strings(final) for secret in secrets)


def _failure_class(payload: bytes) -> str:
    text = payload.decode("utf-8", errors="replace").lower()
    if any(word in text for word in (
        "model_not_found", "model not found", "unknown model", "unsupported model",
        "model is unavailable", "model does not exist",
    )):
        return "model-unavailable"
    if any(word in text for word in ("unauthorized", "authentication failed", "invalid api key", "401")):
        return "authentication"
    if any(word in text for word in ("rate limit", "rate_limit", "429", "insufficient_quota")):
        return "rate-limit"
    if any(word in text for word in ("connection failed", "dns", "could not resolve", "tls error")):
        return "network"
    if "bwrap:" in text or "bubblewrap" in text:
        return "sandbox-setup"
    return "other"


def launch_codex(
    *, worktree: Path, prompt: str, model: str, timeout_seconds: int,
    trace_path: Path, result_path: Path, codex_binary: Path | None = None,
    auth_file: Path | None = None, bubblewrap_binary: Path | None = None,
    reasoning_effort: str = "max", read_only_worktree: bool = False,
) -> LaunchResult:
    """Launch one child; only metadata enters the JSONL trace.

    The caller supplies a unique trace/result pair outside the candidate tree.
    A zero exit requires a session ID and a JSON-object final response.
    """
    worktree = Path(worktree).resolve()
    trace_path, result_path = Path(trace_path).absolute(), Path(result_path).absolute()
    resolved_trace, resolved_result = trace_path.resolve(), result_path.resolve()
    if (trace_path == result_path or resolved_trace == resolved_result or
            worktree in resolved_trace.parents or worktree in resolved_result.parents):
        raise ValueError("evidence paths must be distinct and outside the worktree")
    if not MODEL_NAME.fullmatch(model) or reasoning_effort not in REASONING_EFFORTS:
        raise ValueError("invalid model or reasoning effort")
    if not isinstance(timeout_seconds, int) or timeout_seconds < 1 or timeout_seconds > 24 * 3600:
        raise ValueError("invalid child time limit")
    prompt_bytes = prompt.encode("utf-8")
    if not prompt_bytes or len(prompt_bytes) > MAX_PROMPT_BYTES:
        raise ValueError("invalid child prompt length")
    common_git = _git_metadata(worktree)
    bwrap = Path(bubblewrap_binary or shutil.which("bwrap") or "/nonexistent/bwrap").resolve()
    codex = Path(codex_binary or shutil.which("codex") or "/nonexistent/codex").resolve()
    source_auth = Path(auth_file or Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))) / "auth.json")
    if (worktree in source_auth.absolute().parents or
            worktree in source_auth.resolve().parents or
            worktree in codex.parents or worktree in bwrap.parents):
        raise ValueError("launcher inputs must be outside the child worktree")
    if not bwrap.is_file() or not os.access(bwrap, os.X_OK):
        raise RuntimeError("bubblewrap is required for a child launch")
    if not codex.is_file() or not os.access(codex, os.X_OK):
        raise RuntimeError("Codex executable is unavailable")
    if not source_auth.is_file():
        raise RuntimeError("Codex auth file is unavailable")

    with tempfile.TemporaryDirectory(prefix="agent-runner-home-") as home_name:
        home = Path(home_name)
        (home / ".codex").mkdir(mode=0o700)
        (home / ".cargo").mkdir(mode=0o700)
        secrets = _copy_auth(source_auth, home / ".codex" / "auth.json")
        (home / "result-schema.json").write_text(json.dumps(RESULT_SCHEMA), encoding="utf-8")
        argv, child_argv = _sandbox_argv(
            worktree=worktree, common_git=common_git, private_home=home,
            codex_binary=codex, bwrap_binary=bwrap, model=model, effort=reasoning_effort,
            read_only_worktree=read_only_worktree,
        )
        session_id: str | None = None
        invalid_events = 0
        event_lock = threading.Lock()
        stderr_hash = hashlib.sha256()
        stderr_bytes = 0
        stderr_sample = bytearray()
        event_error_sample = bytearray()
        with _exclusive_file(trace_path) as trace:
            def record(row: dict[str, object]) -> None:
                trace.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")
                trace.flush()

            record({"kind": "launch", "model": model, "reasoning_effort": reasoning_effort,
                    "child_argv": child_argv, "worktree": str(worktree),
                    "read_only_worktree": read_only_worktree})
            try:
                child = subprocess.Popen(
                    argv, cwd=worktree, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, start_new_session=True,
                    env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"},
                )
            except OSError:
                record({"kind": "complete", "status": "launch-failed"})
                return LaunchResult(False, None, False, model, reasoning_effort, None,
                                    "launch-failed", trace_path, result_path)

            def consume_stdout() -> None:
                nonlocal session_id, invalid_events
                assert child.stdout is not None
                with child.stdout:
                    while raw_line := child.stdout.readline(MAX_EVENT_BYTES + 1):
                        if len(raw_line) > MAX_EVENT_BYTES:
                            invalid_events += 1
                            while raw_line and not raw_line.endswith(b"\n"):
                                raw_line = child.stdout.readline(MAX_EVENT_BYTES + 1)
                            continue
                        try:
                            event = json.loads(raw_line)
                        except (ValueError, UnicodeDecodeError):
                            invalid_events += 1
                            continue
                        if not isinstance(event, dict):
                            invalid_events += 1
                            continue
                        name = event.get("type")
                        if not isinstance(name, str) or not EVENT_NAME.fullmatch(name):
                            invalid_events += 1
                            continue
                        row: dict[str, object] = {"kind": "codex_event", "type": name}
                        if name == "thread.started":
                            found = event.get("thread_id")
                            if isinstance(found, str) and re.fullmatch(r"[a-zA-Z0-9-]{1,128}", found):
                                with event_lock:
                                    session_id = found
                                row["session_id"] = found
                        if name in ("error", "turn.failed"):
                            detail = event.get("message") or event.get("error")
                            if isinstance(detail, dict):
                                detail = detail.get("message")
                            if isinstance(detail, str) and len(event_error_sample) < 8192:
                                event_error_sample.extend(detail.encode("utf-8")[:8192 - len(event_error_sample)])
                        record(row)

            def discard_stderr() -> None:
                nonlocal stderr_bytes
                assert child.stderr is not None
                with child.stderr:
                    while chunk := child.stderr.read(65536):
                        stderr_hash.update(chunk)
                        stderr_bytes += len(chunk)
                        if len(stderr_sample) < 8192:
                            stderr_sample.extend(chunk[:8192 - len(stderr_sample)])

            stdout_thread = threading.Thread(target=consume_stdout, daemon=True)
            stderr_thread = threading.Thread(target=discard_stderr, daemon=True)
            stdout_thread.start()
            stderr_thread.start()
            timed_out = False
            try:
                assert child.stdin is not None
                child.stdin.write(prompt_bytes)
                child.stdin.close()
                child.wait(timeout=timeout_seconds)
            except subprocess.TimeoutExpired:
                timed_out = True
                _stop_process_group(child)
            except BaseException:
                _stop_process_group(child)
                raise
            finally:
                stdout_thread.join(timeout=5)
                stderr_thread.join(timeout=5)
            if stdout_thread.is_alive() or stderr_thread.is_alive():
                _kill_process_group(child)
                stdout_thread.join(timeout=5)
                stderr_thread.join(timeout=5)
            exit_code = child.returncode
            error: str | None = None
            if timed_out:
                error = "timeout"
            elif exit_code != 0:
                cause = _failure_class(bytes(stderr_sample + event_error_sample))
                error = cause if cause != "other" else "child-exit"
            elif stdout_thread.is_alive() or stderr_thread.is_alive():
                error = "open-output-pipe"
            elif session_id is None:
                error = "session-id-missing"
            elif invalid_events:
                error = "invalid-jsonl-events"
            else:
                try:
                    fd = os.open(home / "result.json", os.O_RDONLY | os.O_NOFOLLOW)
                    with os.fdopen(fd, "rb") as source:
                        raw = source.read(MAX_RESULT_BYTES + 1)
                    if len(raw) > MAX_RESULT_BYTES:
                        raise ValueError("oversize")
                    final = json.loads(raw)
                    if not _safe_result(final, secrets):
                        raise ValueError("invalid shape")
                    with _exclusive_file(result_path) as destination:
                        json.dump(final, destination, sort_keys=True, separators=(",", ":"))
                        destination.write("\n")
                except (OSError, UnicodeDecodeError, ValueError):
                    error = "invalid-final-result"
            record({"kind": "complete", "status": "ok" if error is None else error,
                    "exit_code": exit_code, "timed_out": timed_out,
                    "session_id": session_id, "invalid_events": invalid_events,
                    "stderr_sha256": stderr_hash.hexdigest(), "stderr_bytes": stderr_bytes,
                    "stderr_class": _failure_class(bytes(stderr_sample + event_error_sample))
                    if exit_code else None})
            return LaunchResult(error is None, exit_code, timed_out, model, reasoning_effort,
                                session_id, error, trace_path, result_path)


def run_proof(
    *, worktree: Path, proof_script: Path, run_id: str,
    argv: list[str], timeout_seconds: int,
    bubblewrap_binary: Path | None = None,
) -> ProofLaunchResult:
    """Run agent-proof and its command without controller credentials or home.

    agent-proof writes its usual revision-bound receipt inside the worktree.
    The controller reads and validates that receipt after the child exits.
    """
    worktree = Path(worktree).resolve()
    common_git = _git_metadata(worktree)
    proof_script = Path(proof_script).resolve()
    bwrap = Path(bubblewrap_binary or shutil.which("bwrap") or "/nonexistent/bwrap").resolve()
    if not bwrap.is_file() or not os.access(bwrap, os.X_OK):
        raise RuntimeError("bubblewrap is required for proof execution")
    if worktree in proof_script.parents or worktree in bwrap.parents:
        raise ValueError("proof launcher inputs must be outside the child worktree")
    info = proof_script.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 1024 * 1024:
        raise ValueError("proof script has unsafe ownership or size")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", run_id):
        raise ValueError("invalid proof run ID")
    if (not isinstance(argv, list) or not argv or
            any(not isinstance(item, str) or not item or "\x00" in item for item in argv)):
        raise ValueError("invalid proof argv")
    if not isinstance(timeout_seconds, int) or timeout_seconds < 1 or timeout_seconds > 24 * 3600:
        raise ValueError("invalid proof time limit")
    with (tempfile.TemporaryDirectory(prefix="agent-runner-proof-home-") as home_name,
          tempfile.TemporaryDirectory(prefix="agent-runner-proof-code-") as code_name):
        home = Path(home_name)
        (home / ".cargo").mkdir(mode=0o700)
        source_fd = os.open(proof_script, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(source_fd, "rb") as source:
            source_bytes = source.read(1024 * 1024 + 1)
        if len(source_bytes) > 1024 * 1024:
            raise ValueError("proof script changed or is too large")
        # Only the read-only bind target is visible to the proof child.
        frozen_script = Path(code_name) / "agent-proof.py"
        frozen_fd = os.open(frozen_script, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                            0o600)
        with os.fdopen(frozen_fd, "wb") as destination:
            destination.write(source_bytes)
        child_argv = ["/usr/bin/python3", "/opt/agent/agent-proof.py",
                      "--run-id", run_id, "--", *argv]
        command = _restricted_argv(
            worktree=worktree, common_git=common_git, private_home=home,
            executable=frozen_script, mounted_executable="/opt/agent/agent-proof.py",
            bwrap_binary=bwrap, child_argv=child_argv, read_only_worktree=False,
        )
        try:
            child = subprocess.Popen(
                command, cwd=worktree, stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                start_new_session=True,
                env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"},
            )
        except OSError:
            return ProofLaunchResult(False, None, False, "launch-failed")
        try:
            child.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            _stop_process_group(child)
            return ProofLaunchResult(False, child.returncode, True, "timeout")
        except BaseException:
            _stop_process_group(child)
            raise
        return ProofLaunchResult(child.returncode == 0, child.returncode, False,
                                 None if child.returncode == 0 else "proof-failed")
