#!/usr/bin/env python3
"""Report newly observed failures for the current main and open PR heads."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import urllib.parse


SCHEMA_VERSION = 1
GITHUB_API_VERSION = "2026-03-10"
SHA = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
REPO = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z")
WORKFLOW_STATUSES = {"completed", "in_progress", "pending", "queued", "requested", "waiting"}
WORKFLOW_CONCLUSIONS = {
    "action_required", "cancelled", "failure", "neutral", "skipped", "stale",
    "startup_failure", "success", "timed_out",
}
ACTIONABLE_CONCLUSIONS = {"action_required", "failure", "startup_failure", "timed_out"}


class IntakeError(Exception):
    pass


def parse_documents(raw, endpoint):
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise IntakeError(f"GitHub returned non-UTF-8 data for {endpoint}") from error

    decoder = json.JSONDecoder()
    documents = []
    offset = 0
    while offset < len(text):
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            break
        try:
            document, offset = decoder.raw_decode(text, offset)
        except json.JSONDecodeError as error:
            raise IntakeError(f"GitHub returned malformed JSON for {endpoint}") from error
        documents.append(document)
    if not documents:
        raise IntakeError(f"GitHub returned an empty response for {endpoint}")
    return documents


def api_get(repo, route, fields=(), paginate=False):
    endpoint = f"repos/{repo}/{route}"
    argv = [
        "gh", "api", "--method", "GET",
        "--header", f"X-GitHub-Api-Version: {GITHUB_API_VERSION}",
    ]
    if paginate:
        argv.append("--paginate")
    for key, value in fields:
        argv.extend(("-F", f"{key}={value}"))
    argv.append(endpoint)
    try:
        result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
    except OSError as error:
        raise IntakeError("cannot start gh api") from error
    if result.returncode != 0:
        raise IntakeError(f"gh api GET failed for {endpoint} (exit {result.returncode})")
    return parse_documents(result.stdout, endpoint)


def one_object(documents, endpoint):
    if len(documents) != 1 or not isinstance(documents[0], dict):
        raise IntakeError(f"unexpected GitHub response shape for {endpoint}")
    return documents[0]


def flatten_lists(documents, endpoint):
    values = []
    for document in documents:
        if not isinstance(document, list):
            raise IntakeError(f"unexpected GitHub response shape for {endpoint}")
        values.extend(document)
    return values


def valid_sha(value, label):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise IntakeError(f"GitHub returned an invalid {label}")
    return value


def positive_int(value, label):
    if isinstance(value, bool) or not isinstance(value, int) or value < 1:
        raise IntakeError(f"GitHub returned an invalid {label}")
    return value


def current_targets(repo):
    main_endpoint = f"repos/{repo}/commits/main"
    main = one_object(api_get(repo, "commits/main"), main_endpoint)
    main_sha = valid_sha(main.get("sha"), "main commit SHA")

    pulls_endpoint = f"repos/{repo}/pulls"
    pulls = flatten_lists(
        api_get(repo, "pulls", (("state", "open"), ("per_page", "100")), paginate=True),
        pulls_endpoint,
    )

    targets = {main_sha: ["main"]}
    for pull in pulls:
        if not isinstance(pull, dict):
            raise IntakeError("GitHub returned an invalid open pull request")
        number = positive_int(pull.get("number"), "pull request number")
        head = pull.get("head")
        if not isinstance(head, dict):
            raise IntakeError("GitHub returned an invalid pull request head")
        head_sha = valid_sha(head.get("sha"), "pull request head SHA")
        branch = clean_label(head.get("ref"), "unknown branch")
        label = f"PR #{number} ({branch})"
        targets.setdefault(head_sha, []).append(label)

    for labels in targets.values():
        labels.sort(key=lambda item: (item != "main", item))
    return targets


def clean_label(value, fallback):
    if not isinstance(value, str) or not value:
        return fallback
    return " ".join("".join(ch if ch.isprintable() else " " for ch in value).split()) or fallback


def run_url(value):
    if not isinstance(value, str):
        raise IntakeError("GitHub returned a workflow run without a URL")
    try:
        parsed = urllib.parse.urlsplit(value)
    except ValueError as error:
        raise IntakeError("GitHub returned an invalid workflow run URL") from error
    if parsed.scheme != "https" or not parsed.netloc or any(ord(ch) < 32 for ch in value):
        raise IntakeError("GitHub returned an invalid workflow run URL")
    return value


def workflow_runs(repo, targets):
    by_id = {}
    for head_sha in sorted(targets, key=lambda sha: ("main" not in targets[sha], sha)):
        endpoint = f"repos/{repo}/actions/runs"
        documents = api_get(
            repo,
            "actions/runs",
            (("head_sha", head_sha), ("per_page", "100")),
            paginate=True,
        )
        advertised_count = None
        returned_ids = set()
        for document in documents:
            count = document.get("total_count") if isinstance(document, dict) else None
            if isinstance(count, bool) or not isinstance(count, int) or count < 0:
                raise IntakeError(f"GitHub returned an invalid total_count for {endpoint}")
            if count >= 1000:
                raise IntakeError(f"GitHub head_sha search reaches its 1,000-result cap for {endpoint}")
            if advertised_count is not None and count != advertised_count:
                raise IntakeError(f"GitHub changed total_count while paginating {endpoint}")
            advertised_count = count
            if not isinstance(document.get("workflow_runs"), list):
                raise IntakeError(f"unexpected GitHub response shape for {endpoint}")
            for run in document["workflow_runs"]:
                if not isinstance(run, dict):
                    raise IntakeError("GitHub returned an invalid workflow run")
                run_id = positive_int(run.get("id"), "workflow run ID")
                returned_ids.add(run_id)
                run_sha = valid_sha(run.get("head_sha"), "workflow run head SHA")
                if run_sha != head_sha:
                    continue
                attempt = positive_int(run.get("run_attempt"), "workflow run attempt")
                workflow_id = positive_int(run.get("workflow_id"), "workflow ID")
                run_number = positive_int(run.get("run_number"), "workflow run number")
                status = run.get("status")
                if not isinstance(status, str) or status not in WORKFLOW_STATUSES:
                    raise IntakeError("GitHub returned an unknown workflow run status")
                if "conclusion" not in run:
                    raise IntakeError("GitHub workflow run is missing conclusion")
                conclusion = run["conclusion"]
                if status == "completed":
                    if not isinstance(conclusion, str) or conclusion not in WORKFLOW_CONCLUSIONS:
                        raise IntakeError("GitHub workflow run has an unknown or missing completed conclusion")
                elif conclusion is not None:
                    raise IntakeError("GitHub workflow run has a conclusion before completion")
                if run_id in by_id:
                    prior_sha, attempts = by_id[run_id]
                    if prior_sha != run_sha:
                        raise IntakeError("GitHub returned one workflow run ID for multiple head SHAs")
                else:
                    attempts = {}
                    by_id[run_id] = (run_sha, attempts)
                prior = attempts.get(attempt)
                if prior is not None and prior != run:
                    raise IntakeError("GitHub returned conflicting copies of one workflow run attempt")
                attempts[attempt] = run
        if advertised_count is None:
            raise IntakeError(f"GitHub omitted total_count for {endpoint}")
        if len(returned_ids) != advertised_count:
            raise IntakeError(
                f"GitHub pagination for {endpoint} returned {len(returned_ids)} unique run IDs, "
                f"but total_count is {advertised_count}"
            )

    by_workflow_head = {}
    for run_sha, attempts in by_id.values():
        run = attempts[max(attempts)]
        workflow_id = positive_int(run.get("workflow_id"), "workflow ID")
        run_number = positive_int(run.get("run_number"), "workflow run number")
        key = (workflow_id, run_sha)
        prior = by_workflow_head.get(key)
        if prior is not None:
            if prior["run_number"] == run_number and prior["run_id"] != run["id"]:
                raise IntakeError("GitHub returned conflicting run IDs for one workflow run number")
            if prior["run_number"] > run_number:
                continue
        by_workflow_head[key] = {
            "workflow_id": workflow_id,
            "run_number": run_number,
            "run_id": positive_int(run.get("id"), "workflow run ID"),
            "attempt": positive_int(run.get("run_attempt"), "workflow run attempt"),
            "head_sha": run_sha,
            "branch": targets[run_sha],
            "workflow": clean_label(run.get("name"), f"workflow {workflow_id}"),
            "status": run.get("status"),
            "conclusion": run.get("conclusion"),
            "url": run_url(run.get("html_url")) if run.get("html_url") else None,
        }
    return sorted(by_workflow_head.values(), key=lambda run: (run["head_sha"], run["workflow_id"]))


def event_key(run):
    return (run["run_id"], run["attempt"], run["head_sha"])


def read_state(path, repo):
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    except FileNotFoundError:
        return None
    except OSError as error:
        raise IntakeError("cannot open intake state") from error
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode):
            raise IntakeError("intake state is not a regular file")
        with os.fdopen(fd, "r", encoding="utf-8") as source:
            fd = -1
            try:
                state = json.load(source)
            except (UnicodeError, json.JSONDecodeError) as error:
                raise IntakeError("intake state is malformed") from error
    finally:
        if fd >= 0:
            os.close(fd)

    if not isinstance(state, dict) or state.get("schema_version") != SCHEMA_VERSION or state.get("repo") != repo:
        raise IntakeError("intake state has an unsupported schema or repository")
    seen = state.get("seen")
    if not isinstance(seen, list):
        raise IntakeError("intake state has an invalid seen list")
    keys = set()
    for item in seen:
        if not isinstance(item, dict):
            raise IntakeError("intake state has an invalid event key")
        run_id = positive_int(item.get("run_id"), "state workflow run ID")
        attempt = positive_int(item.get("attempt"), "state workflow run attempt")
        head_sha = valid_sha(item.get("head_sha"), "state workflow run head SHA")
        keys.add((run_id, attempt, head_sha))
    watermarks = state.get("watermarks")
    if not isinstance(watermarks, list):
        raise IntakeError("intake state has an invalid workflow watermark list")
    latest = {}
    for item in watermarks:
        if not isinstance(item, dict):
            raise IntakeError("intake state has an invalid workflow watermark")
        workflow_id = positive_int(item.get("workflow_id"), "state workflow ID")
        head_sha = valid_sha(item.get("head_sha"), "state workflow head SHA")
        run_number = positive_int(item.get("run_number"), "state workflow run number")
        run_id = positive_int(item.get("run_id"), "state workflow run ID")
        attempt = positive_int(item.get("attempt"), "state workflow run attempt")
        key = (workflow_id, head_sha)
        current = (run_number, run_id, attempt)
        if key in latest and latest[key] != current:
            raise IntakeError("intake state has duplicate workflow watermarks")
        latest[key] = current
    return {"seen": keys, "watermarks": latest}


def open_lock(path):
    flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags, 0o600)
    except OSError as error:
        raise IntakeError("cannot open intake lock") from error
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise IntakeError("intake lock is not a regular file")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise IntakeError("another intake run holds the lock") from error
        return fd
    except Exception:
        os.close(fd)
        raise


def state_directory(path):
    if not path.is_absolute():
        raise IntakeError("--state-dir must be an absolute path")
    repo_root = Path(__file__).resolve().parent.parent
    try:
        path.resolve().relative_to(repo_root)
    except ValueError:
        pass
    else:
        raise IntakeError("--state-dir must be outside the repository checkout")
    try:
        path.mkdir(parents=True, mode=0o700, exist_ok=True)
        info = path.lstat()
    except OSError as error:
        raise IntakeError("cannot create intake state directory") from error
    if not stat.S_ISDIR(info.st_mode):
        raise IntakeError("--state-dir must be a plain directory")
    if hasattr(os, "getuid") and info.st_uid != os.getuid():
        raise IntakeError("--state-dir must be owned by the current user")
    if stat.S_IMODE(info.st_mode) & 0o077:
        raise IntakeError("--state-dir must not be accessible by group or other users")
    return path


def store_report(directory, content):
    report_dir = directory / "reports"
    try:
        report_dir.mkdir(mode=0o700, exist_ok=True)
        info = report_dir.lstat()
        if not stat.S_ISDIR(info.st_mode):
            raise IntakeError("report path is not a plain directory")
        if stat.S_IMODE(info.st_mode) & 0o077:
            raise IntakeError("report directory must not be accessible by group or other users")
    except OSError as error:
        raise IntakeError("cannot create report directory") from error

    data = content.encode("utf-8")
    name = hashlib.sha256(data).hexdigest() + ".json"
    path = report_dir / name
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0), 0o600)
    except FileExistsError:
        try:
            fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        except OSError as error:
            raise IntakeError("cannot verify existing intake report") from error
        with os.fdopen(fd, "rb") as existing:
            if not stat.S_ISREG(os.fstat(existing.fileno()).st_mode) or existing.read() != data:
                raise IntakeError("existing intake report does not match its content hash")
        return
    except OSError as error:
        raise IntakeError("cannot create intake report") from error

    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        dir_fd = os.open(report_dir, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except OSError as error:
        try:
            path.unlink()
        except OSError:
            pass
        raise IntakeError("cannot finish intake report") from error


def write_state(path, repo, seen, watermarks):
    payload = {
        "schema_version": SCHEMA_VERSION,
        "repo": repo,
        "seen": [
            {"run_id": run_id, "attempt": attempt, "head_sha": head_sha}
            for run_id, attempt, head_sha in sorted(seen)
        ],
        "watermarks": [
            {
                "workflow_id": workflow_id,
                "head_sha": head_sha,
                "run_number": current[0],
                "run_id": current[1],
                "attempt": current[2],
            }
            for (workflow_id, head_sha), current in sorted(watermarks.items())
        ],
    }
    data = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8")
    temporary = path.with_name(f".{path.name}.{os.getpid()}.partial")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(temporary, flags, 0o600)
    except FileExistsError as error:
        raise IntakeError("stale temporary intake state exists") from error
    except OSError as error:
        raise IntakeError("cannot prepare intake state update") from error
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        dir_fd = os.open(path.parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(dir_fd)
        finally:
            os.close(dir_fd)
    except OSError as error:
        try:
            temporary.unlink()
        except OSError:
            pass
        raise IntakeError("cannot commit intake state") from error


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, help="GitHub repository as OWNER/REPO")
    parser.add_argument("--state-dir", required=True, type=Path, help="absolute user-local directory outside the checkout")
    parser.add_argument("--report-existing", action="store_true", help="report active failures even when they were already seen")
    args = parser.parse_args(argv)
    if not REPO.fullmatch(args.repo):
        parser.error("--repo must be an OWNER/REPO slug")
    return args


def run(args):
    directory = state_directory(args.state_dir)
    lock_fd = open_lock(directory / "intake.lock")
    try:
        state_path = directory / "state.json"
        state = read_state(state_path, args.repo)
        targets = current_targets(args.repo)
        observations = workflow_runs(args.repo, targets)
        first_run = state is None
        seen = set() if state is None else state["seen"]
        watermarks = {} if state is None else state["watermarks"]
        active_failures = []

        for run in observations:
            marker = (run["workflow_id"], run["head_sha"])
            current = (run["run_number"], run["run_id"], run["attempt"])
            prior = watermarks.get(marker)
            if prior is not None:
                if current[0] < prior[0] or (current[0] == prior[0] and current[2] < prior[2]):
                    continue
                if current[0] == prior[0] and current[1] != prior[1]:
                    raise IntakeError("GitHub returned conflicting current workflow runs")
            if prior is None or current[0] > prior[0] or current[2] > prior[2]:
                watermarks[marker] = current
            if run["status"] == "completed" and run["conclusion"] in ACTIONABLE_CONCLUSIONS:
                if not run["url"]:
                    raise IntakeError("GitHub returned a failed workflow run without a URL")
                active_failures.append(run)

        active_keys = {event_key(run) for run in active_failures}
        if args.report_existing:
            reportable = active_failures
        elif first_run:
            reportable = []
        else:
            reportable = [run for run in active_failures if event_key(run) not in seen]

        if reportable:
            entries = [
                {
                    "url": run["url"],
                    "sha": run["head_sha"],
                    "branches": run["branch"],
                    "workflow": run["workflow"],
                    "workflow_id": run["workflow_id"],
                    "run_id": run["run_id"],
                    "run_number": run["run_number"],
                    "attempt": run["attempt"],
                    "conclusion": run["conclusion"],
                }
                for run in reportable
            ]
            report = {
                "schema_version": SCHEMA_VERSION,
                "repo": args.repo,
                "failures": entries,
            }
            content = json.dumps(report, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
            store_report(directory, content)
            report_path = directory / "reports" / (hashlib.sha256(content.encode("utf-8")).hexdigest() + ".json")
            locator = str(report_path.resolve())
            output = {
                **report,
                "report_path": locator,
                "failures": [dict(entry, report_path=locator) for entry in entries],
            }
            sys.stdout.write(json.dumps(output, indent=2, sort_keys=True, ensure_ascii=True) + "\n")
            sys.stdout.flush()

        write_state(state_path, args.repo, seen | active_keys, watermarks)
    finally:
        os.close(lock_fd)


def main(argv=None):
    try:
        run(parse_args(sys.argv[1:] if argv is None else argv))
    except IntakeError as error:
        print(f"agent-intake: {error}", file=sys.stderr)
        return 1
    except OSError:
        print("agent-intake: local intake I/O failed", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
