#!/usr/bin/env python3
"""Generate a fail-closed, source-provenanced upstream parity queue."""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import math
import os
import re
import stat
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path, PurePosixPath
from typing import Any
from urllib.parse import urlparse

GENERATOR_VERSION = "3.5.0"
CANONICAL_UPSTREAM_URL = "https://github.com/NousResearch/hermes-agent.git"
CANONICAL_FORK_URL = "https://github.com/sheawinkler/hermes-agent-ultra.git"
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
MAX_RECORDED_ERRORS = 100

PATH_TO_TICKET: list[tuple[str, int]] = [
    ("tests/", 20),
    ("skills/", 21),
    ("optional-skills/", 21),
    ("web/", 22),
    ("ui-tui/", 22),
    ("website/", 22),
    ("gateway/platforms/", 23),
    ("plugins/memory/", 23),
    ("environments/benchmarks/", 24),
    ("environments/tool_call_parsers/", 24),
    ("tools/environments/", 24),
    ("packaging/", 25),
    ("scripts/install.sh", 25),
    ("README.md", 25),
    ("Dockerfile", 25),
    ("flake.nix", 25),
    (".github/workflows/", 25),
]

TICKET_NAME = {
    20: "GPAR-01 tests+CI parity",
    21: "GPAR-02 skills parity",
    22: "GPAR-03 UX parity",
    23: "GPAR-04 gateway/plugin-memory parity",
    24: "GPAR-05 environments+parsers+benchmarks",
    25: "GPAR-06 packaging/docs/install parity",
    26: "GPAR-07 upstream queue backfill",
}
PYTHON_TEST_SURFACE_PREFIXES: tuple[str, ...] = ("tests/", "test/")


_PUBLIC_FILE_URL_RE = re.compile(r"file://[^\s\"'`<>]+")
_PUBLIC_ABSOLUTE_PATH_RE = re.compile(r"(?<![A-Za-z0-9_.-])/(?:[^\s\"'`<>]+)")


def sanitize_public_message(message: object) -> str:
    """Redact local file URLs and absolute paths before evidence serialization."""

    text = str(message)
    text = _PUBLIC_FILE_URL_RE.sub("file://<redacted>", text)
    return _PUBLIC_ABSOLUTE_PATH_RE.sub("<path-redacted>", text)


class TruthError(RuntimeError):
    """Raised when a claimed parity source or classification is not trustworthy."""


class ErrorCollector:
    """Keeps machine-readable failures bounded without hiding their total count."""

    def __init__(self) -> None:
        self._errors: list[dict[str, str]] = []
        self._counts: Counter[str] = Counter()

    def add(self, code: str, message: str, **details: str) -> None:
        self._counts[code] += 1
        if len(self._errors) >= MAX_RECORDED_ERRORS:
            return
        row = {"code": code, "message": sanitize_public_message(message)}
        row.update(
            {
                key: sanitize_public_message(value)
                for key, value in details.items()
                if value
            }
        )
        self._errors.append(row)

    @property
    def has_errors(self) -> bool:
        return bool(self._counts)

    def records(self) -> list[dict[str, str]]:
        return list(self._errors)

    def counts(self) -> dict[str, int]:
        return dict(sorted(self._counts.items()))


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def is_full_sha(value: object) -> bool:
    return bool(SHA_RE.fullmatch(str(value).strip().lower()))


def run_git_result(repo_root: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )

def run_git_bytes_result(repo_root: Path, args: list[str]) -> subprocess.CompletedProcess[bytes]:
    """Run a read-only Git query without decoding path-bearing output."""

    return subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=False,
        capture_output=True,
        check=False,
    )


def run_git(repo_root: Path, args: list[str]) -> str:
    proc = run_git_result(repo_root, args)
    if proc.returncode != 0:
        raise TruthError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout.strip()


def canonical_remote_url(url: str) -> str:
    """Normalize supported Git remote spellings to one comparable canonical URL."""

    raw = url.strip()
    if not raw:
        raise TruthError("remote URL is empty")
    if raw.startswith("git@") and ":" in raw:
        host_path = raw[4:]
        host, path = host_path.split(":", 1)
        return _canonical_network_url(host, path)
    parsed = urlparse(raw)
    if parsed.scheme == "file":
        return Path(parsed.path).resolve().as_uri()
    if parsed.scheme:
        if not parsed.hostname:
            raise TruthError(f"remote URL has no host: {raw}")
        return _canonical_network_url(parsed.hostname, parsed.path)
    return Path(raw).resolve().as_uri()


def public_remote_url(value: str | None) -> str | None:
    """Return a public-safe remote identity without exposing local filesystem paths."""

    if not value:
        return None
    try:
        canonical = canonical_remote_url(value)
    except TruthError:
        return "invalid-remote-redacted"
    if urlparse(canonical).scheme == "file":
        return "local-file-remote-redacted"
    return canonical


def _canonical_network_url(host: str, path: str) -> str:
    normalized_path = path.strip().lstrip("/").rstrip("/")
    if not normalized_path:
        raise TruthError("remote URL has no repository path")
    if not normalized_path.endswith(".git"):
        normalized_path += ".git"
    return f"https://{host.lower()}/{normalized_path}"


def remote_url(repo_root: Path, remote: str) -> str | None:
    proc = run_git_result(repo_root, ["remote", "get-url", remote])
    if proc.returncode != 0:
        return None
    value = proc.stdout.strip()
    return value or None


def ensure_remote(
    repo_root: Path,
    remote: str,
    expected_url: str,
    errors: ErrorCollector,
) -> dict[str, Any]:
    configured = remote_url(repo_root, remote)
    if configured is None:
        proc = run_git_result(repo_root, ["remote", "add", remote, expected_url])
        if proc.returncode != 0:
            errors.add(
                "remote_add_failed",
                f"could not add required remote {remote}: {proc.stderr.strip()}",
                remote=remote,
            )
        configured = remote_url(repo_root, remote)

    expected_canonical = canonical_remote_url(expected_url)
    configured_canonical: str | None = None
    if configured:
        try:
            configured_canonical = canonical_remote_url(configured)
        except TruthError as exc:
            errors.add("malformed_remote_url", str(exc), remote=remote)
        if configured_canonical != expected_canonical:
            errors.add(
                "remote_url_mismatch",
                f"remote {remote} is not the declared canonical source",
                remote=remote,
            )
    else:
        errors.add("missing_remote", f"required remote {remote} is unavailable", remote=remote)

    return {
        "remote": remote,
        "configured_url": public_remote_url(configured),
        "canonical_url": public_remote_url(expected_canonical),
        "configured_canonical_url": public_remote_url(configured_canonical),
    }


def repository_is_shallow(repo_root: Path, errors: ErrorCollector) -> bool:
    proc = run_git_result(repo_root, ["rev-parse", "--is-shallow-repository"])
    if proc.returncode != 0:
        errors.add("shallow_state_unavailable", proc.stderr.strip() or "could not determine shallow state")
        return True
    value = proc.stdout.strip().lower()
    if value not in {"true", "false"}:
        errors.add("shallow_state_malformed", f"unexpected shallow state: {value}")
        return True
    return value == "true"


def fetch_remote_branch(
    repo_root: Path,
    remote: str,
    branch: str,
    errors: ErrorCollector,
) -> dict[str, Any]:
    refspec = f"+refs/heads/{branch}:refs/remotes/{remote}/{branch}"
    args = ["fetch", "--no-tags"]
    # A shallow checkout cannot prove a complete range. Ask Git to remove the
    # shallow boundary before accepting either fetched ref as a source.
    if repository_is_shallow(repo_root, errors):
        args.append("--unshallow")
    args.extend([remote, refspec])
    fetched_at = utc_now()
    proc = run_git_result(repo_root, args)
    success = proc.returncode == 0
    if not success:
        errors.add(
            "fetch_failed",
            f"git {' '.join(args)} failed: {proc.stderr.strip()}",
            remote=remote,
        )
    return {
        "attempted": True,
        "success": success,
        "fetched_at_utc": fetched_at if success else None,
        "command": ["git", *args],
    }


def ref_sha(repo_root: Path, ref: str) -> str | None:
    proc = run_git_result(repo_root, ["rev-parse", "--verify", f"{ref}^{{commit}}"])
    if proc.returncode != 0:
        return None
    sha = proc.stdout.strip().lower()
    return sha if is_full_sha(sha) else None


def history_state(repo_root: Path, ref: str, errors: ErrorCollector) -> dict[str, Any]:
    sha = ref_sha(repo_root, ref)
    shallow = repository_is_shallow(repo_root, errors)
    missing_objects: list[str] = []
    if sha:
        proc = run_git_result(repo_root, ["rev-list", "--objects", "--missing=print", ref])
        if proc.returncode != 0:
            errors.add(
                "history_probe_failed",
                f"could not inspect history for {ref}: {proc.stderr.strip()}",
                ref=ref,
            )
        else:
            missing_objects = [
                line[1:] for line in proc.stdout.splitlines() if line.startswith("?")
            ]
    complete = bool(sha) and not shallow and not missing_objects
    if not sha:
        errors.add("missing_ref", f"required ref {ref} does not resolve to a commit", ref=ref)
    if shallow:
        errors.add("shallow_history", f"repository is shallow while evaluating {ref}", ref=ref)
    if missing_objects:
        errors.add(
            "incomplete_history",
            f"history for {ref} has {len(missing_objects)} missing objects",
            ref=ref,
        )
    return {
        "ref": ref,
        "sha": sha,
        "is_shallow_repository": shallow,
        "missing_object_count": len(missing_objects),
        "complete": complete,
    }


def merge_base_result(repo_root: Path, left_ref: str, right_ref: str) -> dict[str, Any]:
    proc = run_git_result(repo_root, ["merge-base", "--all", left_ref, right_ref])
    if proc.returncode == 1:
        return {"status": "absent", "sha": None, "candidates": []}
    if proc.returncode != 0:
        return {
            "status": "error",
            "sha": None,
            "candidates": [],
            "error": proc.stderr.strip() or "git merge-base failed",
        }
    candidates = [line.strip().lower() for line in proc.stdout.splitlines() if line.strip()]
    if len(candidates) == 1 and is_full_sha(candidates[0]):
        return {"status": "present", "sha": candidates[0], "candidates": candidates}
    if len(candidates) > 1:
        return {"status": "ambiguous", "sha": None, "candidates": candidates}
    return {"status": "malformed", "sha": None, "candidates": candidates}


def classify_ticket(files: list[str]) -> int:
    votes: Counter[int] = Counter()
    for path in files:
        for prefix, ticket in PATH_TO_TICKET:
            if path == prefix or path.startswith(prefix):
                votes[ticket] += 1
                break
    return votes.most_common(1)[0][0] if votes else 26


def is_python_test_surface(path: str) -> bool:
    normalized = path.strip()
    if normalized.startswith("./"):
        normalized = normalized[2:]
    return any(normalized.startswith(prefix) for prefix in PYTHON_TEST_SURFACE_PREFIXES)


def commit_is_python_test_only(files: list[str]) -> bool:
    return bool(files) and all(is_python_test_surface(path) for path in files)


def parse_log_blocks(raw: str) -> list[dict[str, Any]]:
    blocks: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    for line in raw.splitlines():
        if line.startswith("__H__"):
            if current is not None:
                blocks.append(current)
            fields = line[len("__H__") :].split("\t", 3)
            if len(fields) != 4 or not is_full_sha(fields[0]) or not is_full_sha(fields[1]):
                raise TruthError(f"malformed git log header: {line!r}")
            parents = [parent for parent in fields[2].split() if is_full_sha(parent)]
            current = {
                "sha": fields[0].lower(),
                "tree_sha": fields[1].lower(),
                "parents": parents,
                "subject": fields[3].strip(),
                "files": [],
            }
            continue
        if current is not None and line:
            current["files"].append(line)
    if current is not None:
        blocks.append(current)
    return blocks


def commit_range_blocks(repo_root: Path, base_ref: str, upstream_ref: str) -> list[dict[str, Any]]:
    raw = run_git(
        repo_root,
        [
            "log",
            "--reverse",
            "--diff-merges=first-parent",
            "--name-only",
            "--format=__H__%H%x09%T%x09%P%x09%s",
            upstream_ref,
            f"^{base_ref}",
        ],
    )
    return parse_log_blocks(raw)


def commit_range_shas(repo_root: Path, base_ref: str, upstream_ref: str) -> list[str]:
    raw = run_git(repo_root, ["rev-list", "--reverse", upstream_ref, f"^{base_ref}"])
    shas = [line.strip().lower() for line in raw.splitlines() if line.strip()]
    if not all(is_full_sha(sha) for sha in shas):
        raise TruthError("git rev-list returned a malformed commit SHA")
    return shas


def local_commit_and_tree_maps(repo_root: Path, ref: str) -> tuple[set[str], dict[str, set[str]]]:
    raw = run_git(repo_root, ["log", "--format=%H%x09%T", ref])
    commits: set[str] = set()
    tree_map: dict[str, set[str]] = defaultdict(set)
    for line in raw.splitlines():
        sha, separator, tree_sha = line.partition("\t")
        sha = sha.strip().lower()
        tree_sha = tree_sha.strip().lower()
        if not separator or not is_full_sha(sha) or not is_full_sha(tree_sha):
            raise TruthError(f"malformed local commit/tree record: {line!r}")
        commits.add(sha)
        tree_map[tree_sha].add(sha)
    return commits, tree_map


def patch_id_map(repo_root: Path, revisions: list[str]) -> dict[str, set[str]]:
    if not revisions:
        return {}
    log_proc = run_git_result(
        repo_root,
        ["log", "--no-merges", "--format=medium", "-p", *revisions],
    )
    if log_proc.returncode != 0:
        raise TruthError(f"git log patch stream failed: {log_proc.stderr.strip()}")
    patch_proc = subprocess.run(
        ["git", "patch-id", "--stable"],
        cwd=repo_root,
        text=True,
        input=log_proc.stdout,
        capture_output=True,
        check=False,
    )
    if patch_proc.returncode != 0:
        raise TruthError(f"git patch-id failed: {patch_proc.stderr.strip()}")
    out: dict[str, set[str]] = defaultdict(set)
    for line in patch_proc.stdout.splitlines():
        parts = line.split()
        if len(parts) != 2 or not is_full_sha(parts[1]):
            raise TruthError(f"malformed git patch-id record: {line!r}")
        patch_id, sha = parts[0].lower(), parts[1].lower()
        if not re.fullmatch(r"[0-9a-f]{40}", patch_id):
            raise TruthError(f"malformed git patch-id: {patch_id!r}")
        out[patch_id].add(sha)
    return out


def reachable_from(repo_root: Path, candidate: str, ref: str) -> bool:
    proc = run_git_result(repo_root, ["merge-base", "--is-ancestor", candidate, ref])
    return proc.returncode == 0


def json_digest(payload: dict[str, Any]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()

LOCAL_SOURCE_SCHEMA_VERSION = 2
LOCAL_SOURCE_DIGEST_ALGORITHM = "sha256-canonical-json-v1"
GENERATED_EVIDENCE_EXCLUSION_SCHEMA_VERSION = 1
CANONICAL_GENERATED_EVIDENCE_PATHS = frozenset(
    {
        ".sync-reports/release-readiness-summary.json",
        ".sync-reports/release-readiness-summary.md",
        "docs/parity/adapter-feature-matrix.json",
        "docs/parity/behavioral-similarity-diff.json",
        "docs/parity/deep-problem-solving-diff.json",
        "docs/parity/divergence-validation.json",
        "docs/parity/global-parity-proof.json",
        "docs/parity/global-parity-proof.md",
        "docs/parity/parity-matrix.json",
        "docs/parity/shared-diff-backlog.json",
        "docs/parity/shared-different-classification.json",
        "docs/parity/sota-harness-matrix.json",
        "docs/parity/test-coverage-audit.json",
        "docs/parity/test-intent-mapping.json",
        "docs/parity/upstream-missing-queue.json",
        "docs/parity/upstream-missing-queue.md",
        "docs/parity/upstream-source-ledger.json",
        "docs/parity/upstream-workstreams.json",
        "docs/parity/workstream-status.json",
    }
)
_CANONICAL_GENERATED_EVIDENCE_PATH_BYTES = frozenset(
    path.encode("utf-8") for path in CANONICAL_GENERATED_EVIDENCE_PATHS
)

def generated_evidence_exclusion_record() -> dict[str, Any]:
    """Describe the fixed output exclusion policy without serializing its paths."""

    return {
        "schema_version": GENERATED_EVIDENCE_EXCLUSION_SCHEMA_VERSION,
        "sha256": json_digest({"paths": sorted(CANONICAL_GENERATED_EVIDENCE_PATHS)}),
    }
_GIT_OBJECT_ID_RE = re.compile(rb"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _is_sha256_digest(value: object) -> bool:
    return isinstance(value, str) and bool(_SHA256_RE.fullmatch(value))


def _is_git_object_id(raw: bytes) -> bool:
    return bool(_GIT_OBJECT_ID_RE.fullmatch(raw))


def _safe_repo_relative_bytes(path: bytes) -> bool:
    return bool(path) and not path.startswith(b"/") and all(
        component not in (b"", b".", b"..") for component in path.split(b"/")
    )


def _is_generated_evidence_path(path: bytes) -> bool:
    return path in _CANONICAL_GENERATED_EVIDENCE_PATH_BYTES


def _nul_records(raw: bytes) -> list[bytes] | None:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        return None
    return raw[:-1].split(b"\0")


def _path_b64(path: bytes) -> str:
    return base64.b64encode(path).decode("ascii")


def _decode_path_b64(value: object) -> bytes | None:
    if not isinstance(value, str):
        return None
    try:
        path = base64.b64decode(value.encode("ascii"), validate=True)
    except (UnicodeEncodeError, ValueError):
        return None
    return path if _safe_repo_relative_bytes(path) else None


def _stable_file_sha256(path: bytes, before: os.stat_result) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    after = os.lstat(path)
    identity = (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_size,
        before.st_mtime_ns,
    )
    after_identity = (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_size,
        after.st_mtime_ns,
    )
    if identity != after_identity:
        raise TruthError("tracked or untracked file changed while its content was being hashed")
    return digest.hexdigest()


def _stable_symlink_sha256(path: bytes, before: os.stat_result) -> str:
    target = os.readlink(path)
    if not isinstance(target, bytes):
        target = os.fsencode(target)
    after = os.lstat(path)
    if (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_mtime_ns,
    ) != (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_mtime_ns,
    ):
        raise TruthError("tracked or untracked symlink changed while its target was being hashed")
    return _sha256_bytes(target)


def _read_index_entries(
    repo_root: Path,
) -> tuple[list[tuple[bytes, dict[str, Any]]], str | None]:
    proc = run_git_bytes_result(repo_root, ["ls-files", "--stage", "-z"])
    records = _nul_records(proc.stdout)
    if proc.returncode != 0 or records is None:
        return [], "git index-entry enumeration failed"
    entries: list[tuple[bytes, dict[str, Any]]] = []
    seen: set[tuple[bytes, int]] = set()
    for raw in records:
        try:
            header, path = raw.split(b"\t", 1)
            mode_raw, object_id_raw, stage_raw = header.split()
            stage = int(stage_raw)
        except (ValueError, UnicodeDecodeError):
            return [], "git returned a malformed index entry"
        if (
            not _safe_repo_relative_bytes(path)
            or not _is_git_object_id(object_id_raw)
            or not re.fullmatch(rb"[0-7]{6}", mode_raw)
            or stage not in (0, 1, 2, 3)
            or (path, stage) in seen
        ):
            return [], "git returned an unsafe or malformed index entry"
        seen.add((path, stage))
        if _is_generated_evidence_path(path):
            continue
        entries.append(
            (
                path,
                {
                    "path_b64": _path_b64(path),
                    "mode": mode_raw.decode("ascii"),
                    "object_id": object_id_raw.decode("ascii"),
                    "stage": stage,
                },
            )
        )
    entries.sort(key=lambda item: (item[0], int(item[1]["stage"])))
    return entries, None


def _read_head_tree_entries(repo_root: Path) -> tuple[list[dict[str, str]], str | None]:
    proc = run_git_bytes_result(repo_root, ["ls-tree", "-r", "-z", "--full-tree", "HEAD"])
    records = _nul_records(proc.stdout)
    if proc.returncode != 0 or records is None:
        return [], "git HEAD tree enumeration failed"
    entries: list[tuple[bytes, dict[str, str]]] = []
    for raw in records:
        try:
            header, path = raw.split(b"\t", 1)
            mode_raw, object_type, object_id_raw = header.split()
        except ValueError:
            return [], "git returned a malformed HEAD tree entry"
        if (
            not _safe_repo_relative_bytes(path)
            or object_type not in (b"blob", b"commit")
            or not _is_git_object_id(object_id_raw)
            or not re.fullmatch(rb"[0-7]{6}", mode_raw)
        ):
            return [], "git returned an unsafe or malformed HEAD tree entry"
        if _is_generated_evidence_path(path):
            continue
        entries.append(
            (
                path,
                {
                    "path_b64": _path_b64(path),
                    "mode": mode_raw.decode("ascii"),
                    "object_id": object_id_raw.decode("ascii"),
                },
            )
        )
    entries.sort(key=lambda item: item[0])
    if len({entry["path_b64"] for _, entry in entries}) != len(entries):
        return [], "git returned duplicate HEAD tree paths"
    return [entry for _, entry in entries], None


def _read_hidden_index_flags(repo_root: Path) -> tuple[list[dict[str, str]], str | None]:
    proc = run_git_bytes_result(repo_root, ["ls-files", "-v", "-z"])
    records = _nul_records(proc.stdout)
    if proc.returncode != 0 or records is None:
        return [], "git index-flag enumeration failed"
    flags: list[dict[str, str]] = []
    for raw in records:
        if len(raw) < 3 or raw[1:2] != b" ":
            return [], "git returned a malformed index flag"
        flag, path = raw[:1], raw[2:]
        if not _safe_repo_relative_bytes(path):
            return [], "git returned an unsafe index-flag path"
        if _is_generated_evidence_path(path):
            continue
        if flag != b"H":
            flags.append({"path_b64": _path_b64(path), "flag": flag.decode("ascii")})
    flags.sort(key=lambda item: _decode_path_b64(item["path_b64"]) or b"")
    return flags, None


def _index_snapshot(
    repo_root: Path,
) -> tuple[dict[str, Any], list[tuple[bytes, dict[str, Any]]], str | None]:
    entries, entry_error = _read_index_entries(repo_root)
    if entry_error:
        return (
            {
                "clean": None,
                "entry_count": None,
                "entries": [],
                "entries_sha256": None,
                "hidden_flags": [],
            },
            [],
            entry_error,
        )
    head_entries, tree_error = _read_head_tree_entries(repo_root)
    flags, flags_error = _read_hidden_index_flags(repo_root)
    serial_entries = [entry for _, entry in entries]
    index = {
        "clean": None,
        "entry_count": len(serial_entries),
        "entries": serial_entries,
        "entries_sha256": json_digest({"entries": serial_entries}),
        "hidden_flags": flags,
    }
    if tree_error or flags_error:
        return index, entries, tree_error or flags_error
    staged_entries = [
        {
            "path_b64": entry["path_b64"],
            "mode": entry["mode"],
            "object_id": entry["object_id"],
        }
        for _, entry in entries
        if entry["stage"] == 0
    ]
    index["clean"] = (
        not flags
        and len(staged_entries) == len(entries)
        and staged_entries == head_entries
    )
    return index, entries, None


def _blob_sha256s(repo_root: Path, object_ids: set[str]) -> tuple[dict[str, str], str | None]:
    if not object_ids:
        return {}, None
    try:
        proc = subprocess.Popen(
            ["git", "cat-file", "--batch"],
            cwd=repo_root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError:
        return {}, "git object-content reader could not start"
    assert proc.stdin is not None and proc.stdout is not None and proc.stderr is not None
    digests: dict[str, str] = {}
    try:
        for object_id in sorted(object_ids):
            proc.stdin.write(object_id.encode("ascii") + b"\n")
            proc.stdin.flush()
            header = proc.stdout.readline()
            fields = header.rstrip(b"\n").split()
            if len(fields) != 3 or fields[0].decode("ascii", "ignore") != object_id:
                return {}, "git returned a malformed tracked object header"
            if fields[1] != b"blob":
                return {}, "tracked index entry does not resolve to a blob"
            try:
                size = int(fields[2])
            except ValueError:
                return {}, "git returned a malformed tracked object size"
            digest = hashlib.sha256()
            remaining = size
            while remaining:
                chunk = proc.stdout.read(min(1024 * 1024, remaining))
                if not chunk:
                    return {}, "git truncated a tracked object"
                digest.update(chunk)
                remaining -= len(chunk)
            if proc.stdout.read(1) != b"\n":
                return {}, "git omitted a tracked object delimiter"
            digests[object_id] = digest.hexdigest()
        proc.stdin.close()
        proc.stderr.read()
        if proc.wait() != 0:
            return {}, "git object-content reader failed"
    except (OSError, ValueError):
        proc.kill()
        proc.wait()
        return {}, "git object-content reader failed"
    return digests, None


def _tracked_worktree_snapshot(
    repo_root: Path,
    index_entries: list[tuple[bytes, dict[str, Any]]],
) -> tuple[dict[str, Any], str | None]:
    stage_zero = [(path, entry) for path, entry in index_entries if entry["stage"] == 0]
    supported_modes = {"100644", "100755", "120000"}
    if any(entry["mode"] not in supported_modes for _, entry in stage_zero):
        return (
            {"clean": None, "entry_count": None, "entries": [], "entries_sha256": None},
            "tracked gitlink or unsupported mode cannot establish worktree provenance",
        )
    blob_digests, blob_error = _blob_sha256s(
        repo_root, {str(entry["object_id"]) for _, entry in stage_zero}
    )
    if blob_error:
        return (
            {"clean": None, "entry_count": None, "entries": [], "entries_sha256": None},
            blob_error,
        )
    root_bytes = os.fsencode(repo_root)
    entries: list[dict[str, Any]] = []
    clean = True
    try:
        for raw_path, index_entry in stage_zero:
            item: dict[str, Any] = {
                "path_b64": _path_b64(raw_path),
                "index_object_id": index_entry["object_id"],
            }
            try:
                before = os.lstat(os.path.join(root_bytes, raw_path))
            except FileNotFoundError:
                item.update({"kind": "missing", "mode": None, "content_sha256": None})
                clean = False
                entries.append(item)
                continue
            absolute_path = os.path.join(root_bytes, raw_path)
            if stat.S_ISREG(before.st_mode):
                item["kind"] = "file"
                item["mode"] = "100755" if before.st_mode & 0o111 else "100644"
                item["content_sha256"] = _stable_file_sha256(absolute_path, before)
            elif stat.S_ISLNK(before.st_mode):
                item["kind"] = "symlink"
                item["mode"] = "120000"
                item["content_sha256"] = _stable_symlink_sha256(absolute_path, before)
            else:
                return (
                    {
                        "clean": None,
                        "entry_count": None,
                        "entries": [],
                        "entries_sha256": None,
                    },
                    "tracked path has an unsupported worktree file type",
                )
            if (
                item["mode"] != index_entry["mode"]
                or item["content_sha256"] != blob_digests[index_entry["object_id"]]
            ):
                clean = False
            entries.append(item)
    except (OSError, TruthError):
        return (
            {"clean": None, "entry_count": None, "entries": [], "entries_sha256": None},
            "tracked worktree content hashing failed",
        )
    return {
        "clean": clean,
        "entry_count": len(entries),
        "entries": entries,
        "entries_sha256": json_digest({"entries": entries}),
    }, None


def _untracked_entries(repo_root: Path) -> tuple[list[dict[str, str]], str | None]:
    """Enumerate untracked paths as sorted raw bytes and hash their current content."""

    proc = run_git_bytes_result(repo_root, ["ls-files", "--others", "--exclude-standard", "-z"])
    records = _nul_records(proc.stdout)
    if proc.returncode != 0 or records is None:
        return [], "git untracked-path enumeration failed"
    paths = [path for path in records if not _is_generated_evidence_path(path)]
    if any(not _safe_repo_relative_bytes(path) for path in paths):
        return [], "git returned an unsafe untracked path"
    if len(paths) != len(set(paths)):
        return [], "git returned duplicate untracked paths"

    root_bytes = os.fsencode(repo_root)
    entries: list[dict[str, str]] = []
    try:
        for raw_path in sorted(paths):
            absolute_path = os.path.join(root_bytes, raw_path)
            before = os.lstat(absolute_path)
            mode = f"{stat.S_IMODE(before.st_mode):04o}"
            if stat.S_ISREG(before.st_mode):
                kind = "file"
                content_sha256 = _stable_file_sha256(absolute_path, before)
            elif stat.S_ISLNK(before.st_mode):
                kind = "symlink"
                content_sha256 = _stable_symlink_sha256(absolute_path, before)
            else:
                raise TruthError("untracked path has an unsupported file type")
            entries.append(
                {
                    "path_b64": _path_b64(raw_path),
                    "kind": kind,
                    "mode": mode,
                    "content_sha256": content_sha256,
                }
            )
    except (OSError, TruthError):
        return [], "untracked content hashing failed"
    return entries, None


def _public_index_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Project private index entries to release-safe aggregate provenance."""

    flags = snapshot.get("hidden_flags")
    flag_count = len(flags) if isinstance(flags, list) else None
    return {
        "clean": snapshot.get("clean"),
        "entry_count": snapshot.get("entry_count"),
        "entries_sha256": snapshot.get("entries_sha256"),
        "hidden_flag_count": flag_count,
        "hidden_flags_detected": bool(flags) if isinstance(flags, list) else None,
    }


def _public_worktree_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Project private tracked-byte entries to release-safe aggregate provenance."""

    return {
        "clean": snapshot.get("clean"),
        "entry_count": snapshot.get("entry_count"),
        "entries_sha256": snapshot.get("entries_sha256"),
    }


def _public_untracked_snapshot(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Project private untracked entries to release-safe aggregate provenance."""

    return {
        "clean": snapshot.get("clean"),
        "count": snapshot.get("count"),
        "entries_sha256": snapshot.get("entries_sha256"),
    }


def local_source_tree_payload(local_source: dict[str, Any]) -> dict[str, Any]:
    """Return the immutable state components covered by tree_state_digest."""

    return {
        "schema_version": local_source.get("schema_version"),
        "head_sha": local_source.get("head_sha"),
        "index": local_source.get("index"),
        "worktree": local_source.get("worktree"),
        "untracked": local_source.get("untracked"),
        "generated_evidence_exclusion": local_source.get("generated_evidence_exclusion"),
    }


def build_local_source_record(
    repo_root: Path,
    *,
    reviewed_dirty_source: bool,
) -> tuple[dict[str, Any], list[str]]:
    """Capture canonical index and actual worktree bytes without mutating Git state."""

    observation_errors: list[str] = []
    head_sha = ref_sha(repo_root, "HEAD")
    if head_sha is None:
        observation_errors.append("local HEAD is missing or malformed")
    index_snapshot, index_entries, index_error = _index_snapshot(repo_root)
    if index_error:
        observation_errors.append(index_error)
    worktree_snapshot, worktree_error = _tracked_worktree_snapshot(repo_root, index_entries)
    if worktree_error:
        observation_errors.append(worktree_error)
    entries, untracked_error = _untracked_entries(repo_root)
    if untracked_error:
        observation_errors.append(untracked_error)
        untracked_snapshot: dict[str, Any] = {
            "clean": None,
            "count": None,
            "entries": [],
            "entries_sha256": None,
        }
    else:
        untracked_snapshot = {
            "clean": not entries,
            "count": len(entries),
            "entries": entries,
            "entries_sha256": json_digest({"entries": entries}),
        }

    index = _public_index_snapshot(index_snapshot)
    worktree = _public_worktree_snapshot(worktree_snapshot)
    untracked = _public_untracked_snapshot(untracked_snapshot)
    cleanliness = (index.get("clean"), worktree.get("clean"), untracked.get("clean"))
    if all(value is True for value in cleanliness) and not observation_errors:
        state = "clean"
    elif any(value is None for value in cleanliness) or observation_errors:
        state = "unavailable"
    else:
        state = "dirty"
    if reviewed_dirty_source and state != "dirty":
        observation_errors.append("--reviewed-dirty-source requires a readable dirty local source")
        state = "unavailable"
    reviewed = reviewed_dirty_source and state == "dirty"
    authority = {
        "mode": (
            "release"
            if state == "clean"
            else "diagnostic_reviewed_dirty_source"
            if reviewed
            else "diagnostic_unreviewed_dirty_source"
            if state == "dirty"
            else "unavailable"
        ),
        "release_eligible": state == "clean",
        "reviewed_dirty_source": reviewed,
    }
    local_source: dict[str, Any] = {
        "schema_version": LOCAL_SOURCE_SCHEMA_VERSION,
        "head_sha": head_sha,
        "index": index,
        "worktree": worktree,
        "untracked": untracked,
        "generated_evidence_exclusion": generated_evidence_exclusion_record(),
        "state": state,
        "authority": authority,
        "tree_state_digest_algorithm": LOCAL_SOURCE_DIGEST_ALGORITHM,
        "tree_state_digest": None,
        "observation_errors": observation_errors,
    }
    local_source["tree_state_digest"] = json_digest(local_source_tree_payload(local_source))
    return local_source, observation_errors


def _is_nonnegative_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _validate_aggregate_component(
    component: object,
    *,
    prefix: str,
    label: str,
    count_field: str,
    required_keys: set[str],
) -> tuple[bool | None, int | None, list[str]]:
    """Validate a public aggregate without accepting private path/content records."""

    errors: list[str] = []
    if not isinstance(component, dict):
        return None, None, [f"{prefix} {label} record is missing or malformed"]
    unexpected = set(component) - required_keys
    if unexpected:
        errors.append(f"{prefix} {label} record contains unsupported private fields")
    clean = component.get("clean")
    if not isinstance(clean, bool):
        errors.append(f"{prefix} {label} cleanliness is missing or malformed")
        clean = None
    count = component.get(count_field)
    if not _is_nonnegative_int(count):
        errors.append(f"{prefix} {label} count is missing or malformed")
        count = None
    if not _is_sha256_digest(component.get("entries_sha256")):
        errors.append(f"{prefix} {label} aggregate digest is missing or malformed")
    return clean, count, errors


def validate_local_source_record(local_source: object) -> list[str]:
    """Fail closed unless public aggregate provenance proves release-clean bytes."""

    prefix = "source ledger local_source"
    if not isinstance(local_source, dict):
        return [f"{prefix} is missing or malformed"]
    errors: list[str] = []
    if local_source.get("schema_version") != LOCAL_SOURCE_SCHEMA_VERSION:
        errors.append(f"{prefix} schema_version is missing or unsupported")
    if not is_full_sha(local_source.get("head_sha")):
        errors.append(f"{prefix} HEAD SHA is missing or malformed")
    if local_source.get("tree_state_digest_algorithm") != LOCAL_SOURCE_DIGEST_ALGORITHM:
        errors.append(f"{prefix} tree-state digest algorithm is missing or unsupported")
    if local_source.get("generated_evidence_exclusion") != generated_evidence_exclusion_record():
        errors.append(f"{prefix} generated-evidence exclusion policy is missing or mismatched")
    if "excluded_generated_evidence_paths" in local_source:
        errors.append(f"{prefix} serializes generated-evidence paths")

    index, index_count, index_errors = _validate_aggregate_component(
        local_source.get("index"),
        prefix=prefix,
        label="index",
        count_field="entry_count",
        required_keys={
            "clean",
            "entry_count",
            "entries_sha256",
            "hidden_flag_count",
            "hidden_flags_detected",
        },
    )
    errors.extend(index_errors)
    index_record = local_source.get("index")
    hidden_flag_count: int | None = None
    if isinstance(index_record, dict):
        candidate = index_record.get("hidden_flag_count")
        if _is_nonnegative_int(candidate):
            hidden_flag_count = candidate
        else:
            errors.append(f"{prefix} index hidden-flag count is missing or malformed")
        detected = index_record.get("hidden_flags_detected")
        if not isinstance(detected, bool):
            errors.append(f"{prefix} index hidden-flag status is missing or malformed")
        elif hidden_flag_count is not None and detected != (hidden_flag_count > 0):
            errors.append(f"{prefix} index hidden-flag status does not match count")
        if hidden_flag_count is not None and hidden_flag_count > 0:
            errors.append(f"{prefix} has hidden or skip-worktree index flags")

    worktree, worktree_count, worktree_errors = _validate_aggregate_component(
        local_source.get("worktree"),
        prefix=prefix,
        label="worktree",
        count_field="entry_count",
        required_keys={"clean", "entry_count", "entries_sha256"},
    )
    errors.extend(worktree_errors)
    if (
        index is True
        and index_count is not None
        and worktree_count is not None
        and index_count != worktree_count
    ):
        errors.append(f"{prefix} clean index and worktree aggregate counts do not match")

    untracked, untracked_count, untracked_errors = _validate_aggregate_component(
        local_source.get("untracked"),
        prefix=prefix,
        label="untracked",
        count_field="count",
        required_keys={"clean", "count", "entries_sha256"},
    )
    errors.extend(untracked_errors)
    if untracked is not None and untracked_count is not None and untracked != (untracked_count == 0):
        errors.append(f"{prefix} untracked cleanliness does not match count")

    observation_errors = local_source.get("observation_errors")
    if (
        not isinstance(observation_errors, list)
        or any(not isinstance(error, str) for error in observation_errors)
        or observation_errors
    ):
        errors.append(f"{prefix} observation errors are missing or nonempty")
    expected_tree_digest = json_digest(local_source_tree_payload(local_source))
    if not _is_sha256_digest(local_source.get("tree_state_digest")):
        errors.append(f"{prefix} tree-state digest is missing or malformed")
    elif local_source.get("tree_state_digest") != expected_tree_digest:
        errors.append(f"{prefix} tree-state digest does not match local state")

    all_clean = (
        index is True
        and worktree is True
        and untracked is True
        and hidden_flag_count == 0
        and observation_errors == []
    )
    is_unavailable = (
        index is None
        or worktree is None
        or untracked is None
        or not isinstance(observation_errors, list)
        or bool(observation_errors)
    )
    state = local_source.get("state")
    expected_state = "unavailable" if is_unavailable else "clean" if all_clean else "dirty"
    if state not in ("clean", "dirty", "unavailable"):
        errors.append(f"{prefix} state is missing or malformed")
    elif state != expected_state:
        errors.append(f"{prefix} state does not match recorded cleanliness")
    authority = local_source.get("authority")
    if not isinstance(authority, dict):
        errors.append(f"{prefix} authority is missing or malformed")
    else:
        reviewed = authority.get("reviewed_dirty_source")
        if not isinstance(reviewed, bool):
            errors.append(f"{prefix} dirty-source review flag is missing or malformed")
        expected_mode = (
            "unavailable"
            if is_unavailable
            else "release"
            if all_clean
            else "diagnostic_reviewed_dirty_source"
            if reviewed is True
            else "diagnostic_unreviewed_dirty_source"
        )
        if authority.get("mode") != expected_mode:
            errors.append(f"{prefix} authority mode does not match local cleanliness")
        if authority.get("release_eligible") is not all_clean:
            errors.append(f"{prefix} authority release eligibility does not match local cleanliness")
        if all_clean and reviewed is not False:
            errors.append(f"{prefix} clean source cannot be marked as reviewed dirty")
    if not all_clean:
        errors.append(f"{prefix} is dirty or unavailable and cannot authorize release")
    return errors


def public_repo_relative_path(path: Path, repo_root: Path) -> str:
    """Return a canonical relative path or a non-path redaction for external inputs."""

    try:
        return path.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        return "external-path-redacted"


def output_path(repo_root: Path, value: Path) -> Path:
    return value if value.is_absolute() else repo_root / value

def generated_evidence_output_error(repo_root: Path, path: Path) -> str | None:
    """Reject noncanonical in-tree outputs so later source resamples stay stable."""

    resolved = output_path(repo_root, path).resolve()
    try:
        relative = resolved.relative_to(repo_root).as_posix()
    except ValueError:
        return None
    if relative in CANONICAL_GENERATED_EVIDENCE_PATHS:
        return None
    return f"in-tree output {relative!r} is not in the canonical generated-evidence set"


def parse_utc_timestamp(value: object) -> str | None:
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(dt.timezone.utc).isoformat()


def safe_repo_artifact_path(value: object) -> str | None:
    raw = str(value).strip()
    if not raw or ":" in raw:
        return None
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or path == PurePosixPath("."):
        return None
    return path.as_posix()


def load_json_from_commit(
    repo_root: Path,
    commit_sha: str,
    artifact_path: str,
) -> dict[str, Any]:
    proc = run_git_result(repo_root, ["show", f"{commit_sha}:{artifact_path}"])
    if proc.returncode != 0:
        raise TruthError(
            f"evidence artifact {artifact_path} is unavailable at {commit_sha}: "
            f"{proc.stderr.strip()}"
        )
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise TruthError(
            f"evidence artifact {artifact_path} at {commit_sha} is malformed JSON: {exc}"
        ) from exc
    if not isinstance(payload, dict):
        raise TruthError(
            f"evidence artifact {artifact_path} at {commit_sha} must be a JSON object"
        )
    return payload


def load_reviewed_upstream_anchor(
    repo_root: Path,
    path: Path,
    fork_ref: str,
    upstream_ref: str,
    expected_upstream_url: str,
    errors: ErrorCollector,
    *,
    required: bool,
) -> dict[str, Any]:
    """Validate the audited anchor that safely bounds disconnected histories."""

    validation_errors: list[str] = []

    def fail(code: str, message: str) -> None:
        public_message = sanitize_public_message(message)
        validation_errors.append(public_message)
        errors.add(code, public_message)

    result: dict[str, Any] = {
        "path": public_repo_relative_path(path, repo_root),
        "valid": False,
        "required": required,
        "anchor_sha": None,
        "review_basis": None,
        "evidence_fork_commit": None,
        "evidence_artifacts": [],
        "validation_errors": validation_errors,
    }
    try:
        relative_path = path.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        fail(
            "reviewed_anchor_path_invalid",
            "reviewed upstream anchor must live inside the repository",
        )
        return result
    if not path.exists():
        if required:
            fail(
                "missing_reviewed_anchor",
                f"reviewed upstream anchor is missing: {relative_path}",
            )
        return result
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail("malformed_reviewed_anchor", f"reviewed upstream anchor is unreadable: {exc}")
        return result
    if not isinstance(payload, dict) or payload.get("schema_version") != 1:
        fail("malformed_reviewed_anchor", "reviewed upstream anchor schema_version must be 1")
        return result

    anchor_sha = str(payload.get("anchor_sha", "")).strip().lower()
    result["anchor_sha"] = anchor_sha if is_full_sha(anchor_sha) else None
    if not is_full_sha(anchor_sha):
        fail("malformed_reviewed_anchor", "reviewed upstream anchor_sha must be a full SHA")

    anchor_url = str(payload.get("canonical_upstream_url", "")).strip()
    try:
        canonical_anchor_url = canonical_remote_url(anchor_url)
    except TruthError as exc:
        canonical_anchor_url = None
        fail("malformed_reviewed_anchor", f"reviewed anchor upstream URL is malformed: {exc}")
    if canonical_anchor_url and canonical_anchor_url != expected_upstream_url:
        fail(
            "reviewed_anchor_url_mismatch",
            "reviewed anchor canonical upstream URL does not match the fetched upstream source",
        )
    if parse_utc_timestamp(payload.get("reviewed_at_utc")) is None:
        fail(
            "malformed_reviewed_anchor",
            "reviewed upstream anchor reviewed_at_utc must be timezone-aware ISO-8601",
        )
    review_basis = payload.get("review_basis")
    if (
        not isinstance(review_basis, dict)
        or not isinstance(review_basis.get("kind"), str)
        or not review_basis["kind"].strip()
        or not isinstance(review_basis.get("semantic_evidence"), str)
        or not review_basis["semantic_evidence"].strip()
    ):
        fail(
            "malformed_reviewed_anchor",
            "reviewed upstream anchor requires an explicit review basis and semantic evidence",
        )
    elif isinstance(review_basis, dict):
        result["review_basis"] = {
            "kind": review_basis["kind"].strip(),
            "semantic_evidence": review_basis["semantic_evidence"].strip(),
        }

    evidence = payload.get("evidence")
    if not isinstance(evidence, dict):
        fail("malformed_reviewed_anchor", "reviewed upstream anchor evidence must be an object")
        return result
    evidence_commit = str(evidence.get("fork_commit", "")).strip().lower()
    result["evidence_fork_commit"] = evidence_commit if is_full_sha(evidence_commit) else None
    if not is_full_sha(evidence_commit):
        fail(
            "malformed_reviewed_anchor",
            "reviewed upstream anchor evidence.fork_commit must be a full SHA",
        )
    artifacts = evidence.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        fail(
            "malformed_reviewed_anchor",
            "reviewed upstream anchor evidence.artifacts must be a non-empty list",
        )
        artifacts = []

    if is_full_sha(anchor_sha) and not reachable_from(repo_root, anchor_sha, upstream_ref):
        fail(
            "reviewed_anchor_not_upstream_ancestor",
            "reviewed upstream anchor is not reachable from the freshly fetched upstream ref",
        )
    if is_full_sha(evidence_commit) and not reachable_from(repo_root, evidence_commit, fork_ref):
        fail(
            "reviewed_anchor_evidence_unreachable",
            "reviewed anchor evidence fork commit is not reachable from the freshly fetched fork ref",
        )

    for item in artifacts:
        if not isinstance(item, dict):
            fail("malformed_reviewed_anchor", "reviewed anchor evidence artifact must be an object")
            continue
        artifact_path = safe_repo_artifact_path(item.get("path"))
        classification = item.get("anchor_classification")
        if artifact_path is None or not isinstance(classification, dict):
            fail(
                "malformed_reviewed_anchor",
                "reviewed anchor evidence artifact requires a safe path and anchor_classification",
            )
            continue
        expected_sha = str(classification.get("sha", "")).strip().lower()
        expected_disposition = str(classification.get("disposition", "")).strip()
        expected_rule = str(classification.get("classification_rule", "")).strip()
        if (
            expected_sha != anchor_sha
            or not is_full_sha(expected_sha)
            or not expected_disposition
            or not expected_rule
        ):
            fail(
                "malformed_reviewed_anchor",
                "anchor evidence classification must use the anchor full SHA, disposition, and rule",
            )
            continue
        result["evidence_artifacts"].append(
            {
                "path": artifact_path,
                "anchor_classification": {
                    "sha": expected_sha,
                    "disposition": expected_disposition,
                    "classification_rule": expected_rule,
                },
            }
        )
        if not is_full_sha(evidence_commit):
            continue
        try:
            evidence_payload = load_json_from_commit(
                repo_root,
                evidence_commit,
                artifact_path,
            )
        except TruthError as exc:
            fail("reviewed_anchor_evidence_missing", str(exc))
            continue
        commits = evidence_payload.get("commits")
        if not isinstance(commits, list):
            fail(
                "reviewed_anchor_evidence_malformed",
                f"evidence artifact {artifact_path} has no commits list",
            )
            continue
        matches = [
            row
            for row in commits
            if isinstance(row, dict) and str(row.get("sha", "")).strip().lower() == anchor_sha
        ]
        if len(matches) != 1:
            fail(
                "reviewed_anchor_evidence_mismatch",
                f"evidence artifact {artifact_path} must name the anchor exactly once",
            )
            continue
        match = matches[0]
        if (
            match.get("disposition") != expected_disposition
            or match.get("classification_rule") != expected_rule
        ):
            fail(
                "reviewed_anchor_evidence_mismatch",
                f"evidence artifact {artifact_path} does not classify the anchor as recorded",
            )

    result["valid"] = not validation_errors
    return result


def load_overrides(path: Path, errors: ErrorCollector) -> dict[str, dict[str, Any]]:
    """Load only fully proven per-SHA classifications.

    Subject patterns, prefix SHA matching, and inherited queue state are
    intentionally unsupported: each retained disposition must identify one
    immutable upstream commit and one local provenance commit.
    """

    if not path.exists():
        return {}
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        errors.add("malformed_overrides", f"could not parse overrides: {exc}")
        return {}
    if not isinstance(payload, dict):
        errors.add("malformed_overrides", "overrides root must be an object")
        return {}
    raw = payload.get("sha", {})
    if not isinstance(raw, dict):
        errors.add("malformed_overrides", "overrides.sha must be an object")
        return {}
    subject_patterns = payload.get("subject_patterns", [])
    if subject_patterns:
        errors.add(
            "unsupported_subject_override",
            "subject-pattern dispositions are not source-provenanced",
        )

    overrides: dict[str, dict[str, Any]] = {}
    for key, value in raw.items():
        sha = str(key).strip().lower()
        if not is_full_sha(sha):
            errors.add(
                "malformed_override_sha",
                "override keys must be exact 40-character commit SHAs",
                sha=sha,
            )
            continue
        if sha in overrides:
            errors.add("duplicate_override_sha", "duplicate exact override SHA", sha=sha)
            continue
        if not isinstance(value, dict):
            errors.add("malformed_override", "override must be an object", sha=sha)
            continue
        disposition = str(value.get("disposition", "")).strip().lower()
        owner = str(value.get("owner", "")).strip()
        notes = str(value.get("notes", "")).strip()
        provenance = value.get("provenance")
        semantic = value.get("semantic_equivalence")
        if disposition not in {"ported", "superseded"}:
            errors.add("malformed_override", "override disposition must be ported or superseded", sha=sha)
            continue
        if not owner or not notes:
            errors.add("malformed_override", "override requires non-empty owner and notes", sha=sha)
            continue
        if not isinstance(provenance, dict) or not isinstance(semantic, dict):
            errors.add(
                "unproven_override",
                "override requires provenance and semantic_equivalence objects",
                sha=sha,
            )
            continue
        upstream_sha = str(provenance.get("upstream_sha", "")).strip().lower()
        fork_sha = str(provenance.get("fork_sha", "")).strip().lower()
        provenance_evidence = str(provenance.get("evidence", "")).strip()
        semantic_method = str(semantic.get("method", "")).strip()
        semantic_evidence = str(semantic.get("evidence", "")).strip()
        if (
            upstream_sha != sha
            or not is_full_sha(fork_sha)
            or not provenance_evidence
            or not semantic_method
            or not semantic_evidence
        ):
            errors.add(
                "unproven_override",
                "override provenance or semantic evidence is incomplete",
                sha=sha,
            )
            continue
        overrides[sha] = {
            "disposition": disposition,
            "owner": owner,
            "notes": notes,
            "provenance": {
                "upstream_sha": upstream_sha,
                "fork_sha": fork_sha,
                "evidence": provenance_evidence,
            },
            "semantic_equivalence": {
                "method": semantic_method,
                "evidence": semantic_evidence,
            },
        }
    return overrides


def load_approved_divergence_prefixes(
    path: Path, errors: ErrorCollector
) -> tuple[str, ...]:
    """Load exact-file and directory-prefix approvals from the divergence registry."""

    if not path.exists():
        return ()
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        errors.add(
            "intentional_divergence_registry_invalid",
            f"cannot load intentional divergence registry: {exc}",
        )
        return ()
    if not isinstance(payload, dict) or not isinstance(payload.get("items"), list):
        errors.add(
            "intentional_divergence_registry_invalid",
            "intentional divergence registry must contain an items array",
        )
        return ()
    prefixes: set[str] = set()
    for item in payload["items"]:
        if not isinstance(item, dict) or item.get("status") != "approved":
            continue
        raw_prefixes = item.get("path_prefixes", [])
        if not isinstance(raw_prefixes, list):
            errors.add(
                "intentional_divergence_registry_invalid",
                "approved divergence path_prefixes must be an array",
            )
            continue
        for raw_prefix in raw_prefixes:
            prefix = str(raw_prefix)
            pure = PurePosixPath(prefix)
            if (
                not prefix
                or prefix.startswith("/")
                or ".." in pure.parts
                or "." in pure.parts
                or "\\" in prefix
                or prefix != prefix.strip()
            ):
                errors.add(
                    "intentional_divergence_registry_invalid",
                    f"unsafe approved divergence prefix: {prefix!r}",
                )
                continue
            prefixes.add(prefix)
    return tuple(sorted(prefixes))


def path_has_approved_prefix(path: str, prefixes: tuple[str, ...]) -> bool:
    pure = PurePosixPath(path)
    if (
        not path
        or path.startswith("/")
        or path.startswith("./")
        or "\\" in path
        or ".." in pure.parts
        or "." in pure.parts
    ):
        return False
    return any(
        path.startswith(prefix) if prefix.endswith("/") else path == prefix
        for prefix in prefixes
    )


def validate_override(
    repo_root: Path,
    override: dict[str, Any],
    upstream_sha: str,
    upstream_tree_sha: str,
    upstream_files: list[str],
    approved_divergence_prefixes: tuple[str, ...],
    fork_ref: str,
    local_patch_candidates: dict[str, set[str]],
    upstream_patch_id: str | None,
    local_tree_candidates: dict[str, set[str]],
    errors: ErrorCollector,
) -> tuple[bool, str]:
    provenance = override["provenance"]
    semantic = override["semantic_equivalence"]
    fork_sha = str(provenance["fork_sha"])
    method = str(semantic["method"])
    if not reachable_from(repo_root, fork_sha, fork_ref):
        errors.add(
            "override_provenance_missing",
            "override fork provenance commit is not reachable from fetched fork ref",
            sha=upstream_sha,
        )
        return False, ""
    if override["disposition"] == "superseded":
        if method not in {"intentional_divergence", "behavioral_contract"}:
            errors.add(
                "override_semantic_method_invalid",
                "superseded override requires intentional_divergence or behavioral_contract evidence",
                sha=upstream_sha,
            )
            return False, ""
        if method == "intentional_divergence" and not upstream_files:
            errors.add(
                "override_divergence_paths_missing",
                "intentional_divergence override requires non-empty changed-path evidence",
                sha=upstream_sha,
            )
            return False, ""
        if method == "intentional_divergence":
            unapproved_paths = [
                path
                for path in upstream_files
                if not path_has_approved_prefix(path, approved_divergence_prefixes)
            ]
            if unapproved_paths:
                errors.add(
                    "override_divergence_path_unapproved",
                    "intentional_divergence override changes paths outside approved "
                    f"registry prefixes: {', '.join(unapproved_paths[:5])}",
                    sha=upstream_sha,
                )
                return False, ""
        return True, "explicit_supersede_override"
    if method == "exact_sha" and fork_sha == upstream_sha:
        return True, "explicit_exact_sha_override"
    if method == "patch_id" and upstream_patch_id:
        candidates = local_patch_candidates.get(upstream_patch_id, set())
        if candidates == {fork_sha}:
            return True, "explicit_patch_id_override"
    if method == "tree_content" and fork_sha in local_tree_candidates.get(upstream_tree_sha, set()):
        return True, "explicit_tree_content_override"
    errors.add(
        "override_semantic_mismatch",
        "ported override provenance does not match exact SHA, patch-id, or tree-content evidence",
        sha=upstream_sha,
    )
    return False, ""


def append_note(existing: str, addition: str) -> str:
    return f"{existing} | {addition}" if existing else addition


def classify_blocks(
    repo_root: Path,
    blocks: list[dict[str, Any]],
    fork_ref: str,
    local_commits: set[str],
    local_tree_map: dict[str, set[str]],
    local_patch_map: dict[str, set[str]],
    upstream_patch_map: dict[str, set[str]],
    overrides: dict[str, dict[str, Any]],
    approved_divergence_prefixes: tuple[str, ...],
    errors: ErrorCollector,
) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    patch_id_by_upstream: dict[str, str] = {}
    for patch_id, shas in upstream_patch_map.items():
        for sha in shas:
            patch_id_by_upstream[sha] = patch_id

    for block in blocks:
        sha = str(block["sha"])
        files = sorted(set(str(path) for path in block["files"]))
        tree_sha = str(block["tree_sha"])
        disposition = "pending"
        owner = ""
        notes = ""
        rule = "unclassified_delta"
        semantic: dict[str, Any] | None = None
        patch_id = patch_id_by_upstream.get(sha)

        override = overrides.get(sha)
        if override is not None:
            valid, override_rule = validate_override(
                repo_root,
                override,
                sha,
                tree_sha,
                files,
                approved_divergence_prefixes,
                fork_ref,
                local_patch_map,
                patch_id,
                local_tree_map,
                errors,
            )
            if valid:
                disposition = str(override["disposition"])
                owner = str(override["owner"])
                notes = str(override["notes"])
                rule = override_rule
                semantic = {
                    **override["semantic_equivalence"],
                    "provenance": override["provenance"],
                }

        if disposition == "pending" and sha in local_commits:
            disposition = "ported"
            rule = "exact_sha_equivalence"
            notes = append_note(notes, f"exact upstream SHA is reachable from {fork_ref}")
            semantic = {"method": "exact_sha", "fork_sha": sha}

        tree_candidates = local_tree_map.get(tree_sha, set())
        if disposition == "pending" and tree_candidates:
            if len(tree_candidates) == 1:
                fork_sha = next(iter(tree_candidates))
                disposition = "ported"
                rule = "tree_content_equivalence"
                notes = append_note(notes, "full Git tree object matches a fetched fork commit")
                semantic = {"method": "tree_content", "fork_sha": fork_sha, "tree_sha": tree_sha}
            else:
                errors.add(
                    "ambiguous_tree_mapping",
                    "upstream tree content maps to multiple fetched fork commits",
                    sha=sha,
                )

        if disposition == "pending" and patch_id:
            patch_candidates = local_patch_map.get(patch_id, set())
            if len(patch_candidates) == 1:
                fork_sha = next(iter(patch_candidates))
                disposition = "ported"
                rule = "patch_id_equivalence"
                notes = append_note(notes, "stable patch-id maps to exactly one fetched fork commit")
                semantic = {"method": "patch_id", "fork_sha": fork_sha, "patch_id": patch_id}
            elif len(patch_candidates) > 1:
                errors.add(
                    "ambiguous_patch_mapping",
                    "upstream patch-id maps to multiple fetched fork commits",
                    sha=sha,
                )

        row = {
            "sha": sha,
            "tree_sha": tree_sha,
            "parents": list(block["parents"]),
            "is_merge_commit": len(block["parents"]) > 1,
            "subject": str(block["subject"]),
            "target_ticket": classify_ticket(files),
            "target_ticket_name": TICKET_NAME.get(classify_ticket(files), "unknown"),
            "files_touched": len(files),
            "files": files,
            "files_sample": files[:20],
            "disposition": disposition,
            "owner": owner,
            "notes": notes,
            "classification_rule": rule,
            "semantic_equivalence": semantic,
        }
        rows.append(row)
    return rows


def render_markdown(payload: dict[str, Any]) -> str:
    summary = payload["summary"]
    truth = payload["truth"]
    ledger = payload["source_ledger"]
    lines = [
        "# Upstream Missing Patch Queue",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        f"Truth status: **{'PASS' if truth['pass'] else 'FAIL'}**",
        "",
        f"- Canonical ledger: `{ledger['path']}` (`{ledger['sha256']}`).",
        f"- Fetched range: `{ledger['fork_sha']}..{ledger['upstream_sha']}`.",
        f"- Complete range commits: `{summary['total_commits']}`; unowned: `{summary['unowned_divergence_count']}`.",
        "",
        "| Disposition | Commit Count |",
        "| --- | ---: |",
    ]
    for disposition, count in sorted(summary["by_disposition"].items()):
        lines.append(f"| {disposition} | {count} |")
    lines.extend(["", "## Source and Classification Errors", ""])
    if truth["error_count"]:
        lines.extend(["| Code | Count |", "| --- | ---: |"])
        for code, count in sorted(truth["error_counts"].items()):
            lines.append(f"| {code} | {count} |")
    else:
        lines.append("None.")
    lines.extend(["", "## First 100 Pending Commits", "", "| SHA | Ticket | Subject |", "| --- | ---: | --- |"])
    pending = 0
    for row in payload["commits"]:
        if row["disposition"] != "pending":
            continue
        subject = str(row["subject"]).replace("|", "\\|")
        lines.append(f"| `{row['sha'][:12]}` | #{row['target_ticket']} | {subject} |")
        pending += 1
        if pending == 100:
            break
    if not pending:
        lines.append("| _(none)_ | - | - |")
    lines.append("")
    return "\n".join(lines)


def build_source_ledger(args: argparse.Namespace, repo_root: Path, errors: ErrorCollector) -> tuple[dict[str, Any], str, str]:
    fork_remote = ensure_remote(repo_root, args.fork_remote, args.fork_url, errors)
    upstream_remote = ensure_remote(repo_root, args.upstream_remote, args.upstream_url, errors)
    fork_ref = f"{args.fork_remote}/{args.fork_branch}"
    upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"

    if args.no_fetch:
        errors.add("fetch_required", "--no-fetch cannot establish fresh parity source provenance")
        fork_fetch = {"attempted": False, "success": False, "fetched_at_utc": None, "command": []}
        upstream_fetch = {"attempted": False, "success": False, "fetched_at_utc": None, "command": []}
    else:
        fork_fetch = fetch_remote_branch(repo_root, args.fork_remote, args.fork_branch, errors)
        upstream_fetch = fetch_remote_branch(repo_root, args.upstream_remote, args.upstream_branch, errors)

    fork_history = history_state(repo_root, fork_ref, errors)
    upstream_history = history_state(repo_root, upstream_ref, errors)
    requested_local_sha = ref_sha(repo_root, args.local_ref) if args.local_ref else None
    if args.local_ref and requested_local_sha is None:
        errors.add("missing_local_ref", f"requested local ref {args.local_ref} does not resolve", ref=args.local_ref)
    if requested_local_sha and fork_history["sha"] and requested_local_sha != fork_history["sha"]:
        errors.add(
            "fork_ref_mismatch",
            "requested local ref does not equal the freshly fetched fork SHA",
            ref=args.local_ref,
        )

    merge_base = merge_base_result(repo_root, fork_ref, upstream_ref)

    freshness_state = "fresh" if fork_fetch["success"] and upstream_fetch["success"] else "unverified"
    ledger = {
        "schema_version": 2,
        "generator": {
            "name": "generate-upstream-patch-queue.py",
            "version": GENERATOR_VERSION,
        },
        "generated_at_utc": utc_now(),
        "sources": {
            "fork": {
                **fork_remote,
                "branch": args.fork_branch,
                "ref": fork_ref,
                "fetched_sha": fork_history["sha"] if fork_fetch["success"] else None,
                "observed_sha": fork_history["sha"],
                "fetch": fork_fetch,
                "history": fork_history,
            },
            "upstream": {
                **upstream_remote,
                "branch": args.upstream_branch,
                "ref": upstream_ref,
                "fetched_sha": upstream_history["sha"] if upstream_fetch["success"] else None,
                "observed_sha": upstream_history["sha"],
                "fetch": upstream_fetch,
                "history": upstream_history,
            },
        },
        "freshness": {
            "required_within_seconds": args.max_source_age_seconds,
            "state": freshness_state,
            "freshly_fetched": freshness_state == "fresh",
        },
        "requested_local_ref": args.local_ref,
        "requested_local_sha": requested_local_sha,
        "merge_base": merge_base,
        "reviewed_anchor": None,
        "range": {
            "fork_ref": fork_ref,
            "upstream_ref": upstream_ref,
            "commit_shas": [],
            "commit_count": 0,
            "sha256": None,
            "complete": False,
        },
        "source_errors": [],
        "source_error_counts": {},
        "source_valid": False,
    }
    return ledger, fork_ref, upstream_ref


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument("--local-ref", default=None, help="Must equal freshly fetched fork SHA when supplied")
    parser.add_argument(
        "--reviewed-dirty-source",
        action="store_true",
        help="Record a dirty source diagnostically; it remains ineligible for release authority",
    )
    parser.add_argument("--fork-remote", default="origin")
    parser.add_argument("--fork-url", default=CANONICAL_FORK_URL)
    parser.add_argument("--fork-branch", default="main")
    parser.add_argument("--upstream-remote", default="upstream")
    parser.add_argument("--upstream-url", default=CANONICAL_UPSTREAM_URL)
    parser.add_argument("--upstream-branch", default="main")
    parser.add_argument("--no-fetch", action="store_true")
    parser.add_argument(
        "--max-source-age-seconds",
        type=int,
        default=3600,
        help="Recorded freshness bound consumed by release readiness validation",
    )
    parser.add_argument(
        "--max-commits",
        type=int,
        default=0,
        help="Legacy diagnostic limiter; any nonzero value makes the result fail closed",
    )
    parser.add_argument("--out-json", default="docs/parity/upstream-missing-queue.json", type=Path)
    parser.add_argument("--out-md", default="docs/parity/upstream-missing-queue.md", type=Path)
    parser.add_argument(
        "--out-source-ledger",
        default="docs/parity/upstream-source-ledger.json",
        type=Path,
    )
    parser.add_argument("--overrides", default="docs/parity/queue-overrides.json", type=Path)
    parser.add_argument(
        "--reviewed-anchor",
        default="docs/parity/reviewed-upstream-anchor.json",
        type=Path,
        help="Durable reviewed upstream anchor used to bound disconnected histories",
    )
    parser.add_argument(
        "--intentional-divergence",
        default="docs/parity/intentional-divergence.json",
        type=Path,
        help="Approved path prefixes for intentional divergence overrides",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    errors = ErrorCollector()
    if args.max_source_age_seconds <= 0:
        errors.add("invalid_freshness_bound", "max source age must be positive")
    for label, candidate in (
        ("queue JSON", args.out_json),
        ("queue markdown", args.out_md),
        ("source ledger", args.out_source_ledger),
    ):
        if output_error := generated_evidence_output_error(repo_root, candidate):
            errors.add("noncanonical_generated_evidence_output", f"{label}: {output_error}")

    try:
        ledger, fork_ref, upstream_ref = build_source_ledger(args, repo_root, errors)
    except (OSError, TruthError) as exc:
        errors.add("source_setup_failed", str(exc))
        fork_ref = f"{args.fork_remote}/{args.fork_branch}"
        upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"
        ledger = {
            "schema_version": 2,
            "generator": {"name": "generate-upstream-patch-queue.py", "version": GENERATOR_VERSION},
            "generated_at_utc": utc_now(),
            "sources": {},
            "freshness": {"required_within_seconds": args.max_source_age_seconds, "state": "unverified", "freshly_fetched": False},
            "requested_local_ref": args.local_ref,
            "requested_local_sha": None,
            "merge_base": {"status": "error", "sha": None, "candidates": []},
            "reviewed_anchor": None,
            "range": {"fork_ref": fork_ref, "upstream_ref": upstream_ref, "commit_shas": [], "commit_count": 0, "sha256": None, "complete": False},
            "source_errors": [],
            "source_error_counts": {},
            "source_valid": False,
        }

    local_source, local_source_errors = build_local_source_record(
        repo_root,
        reviewed_dirty_source=args.reviewed_dirty_source,
    )
    ledger["local_source"] = local_source
    ledger["local_source_sha256"] = json_digest(local_source)
    for issue in local_source_errors:
        errors.add("local_source_unavailable", issue)
    local_state = local_source.get("state")
    if local_state == "dirty":
        errors.add(
            "dirty_local_source",
            "local index, tracked worktree, or untracked source state is dirty",
        )
    elif local_state != "clean":
        errors.add("local_source_unavailable", "local source state is not readable and clean")
    local_head_sha = local_source.get("head_sha")
    expected_fork_sha = ledger.get("sources", {}).get("fork", {}).get("fetched_sha")
    requested_local_sha = ledger.get("requested_local_sha")
    for expected_sha, label in (
        (expected_fork_sha, "freshly fetched fork"),
        (requested_local_sha, "requested local ref"),
    ):
        if is_full_sha(expected_sha) and local_head_sha != expected_sha:
            errors.add(
                "local_head_mismatch",
                f"local HEAD does not match the {label} SHA",
            )

    merge_base = ledger.get("merge_base", {})
    anchor_path = output_path(repo_root, args.reviewed_anchor).resolve()
    try:
        expected_upstream_url = canonical_remote_url(args.upstream_url)
    except TruthError:
        expected_upstream_url = ""
    reviewed_anchor = load_reviewed_upstream_anchor(
        repo_root,
        anchor_path,
        fork_ref,
        upstream_ref,
        expected_upstream_url,
        errors,
        required=not (
            isinstance(merge_base, dict)
            and merge_base.get("status") == "present"
            and is_full_sha(merge_base.get("sha"))
        ),
    )
    ledger["reviewed_anchor"] = reviewed_anchor

    blocks: list[dict[str, Any]] = []
    range_shas: list[str] = []
    range_base: str | None = None
    range_mode = "unavailable"
    histories_complete = bool(
        ledger.get("sources", {}).get("fork", {}).get("history", {}).get("complete")
        and ledger.get("sources", {}).get("upstream", {}).get("history", {}).get("complete")
    )
    if isinstance(merge_base, dict) and merge_base.get("status") == "present" and is_full_sha(
        merge_base.get("sha")
    ):
        range_base = str(merge_base["sha"])
        range_mode = "merge_base"
    elif reviewed_anchor["valid"]:
        range_base = str(reviewed_anchor["anchor_sha"])
        range_mode = "reviewed_upstream_anchor"
        if isinstance(merge_base, dict):
            merge_base["accepted_by_reviewed_anchor"] = True
    else:
        errors.add(
            "missing_merge_base",
            "fetched fork and upstream have no usable merge base and no validated reviewed anchor",
        )

    if range_base and ref_sha(repo_root, fork_ref) and ref_sha(repo_root, upstream_ref):
        try:
            range_shas = commit_range_shas(repo_root, range_base, upstream_ref)
            blocks = commit_range_blocks(repo_root, range_base, upstream_ref)
            block_shas = [str(block["sha"]) for block in blocks]
            if block_shas != range_shas:
                errors.add(
                    "incomplete_range",
                    "git log detail rows do not exactly match the calculated canonical range",
                )
            if args.max_commits > 0:
                errors.add(
                    "incomplete_range_requested",
                    "--max-commits omits part of the canonical commit range",
                )
                blocks = blocks[: args.max_commits]
        except TruthError as exc:
            errors.add("range_enumeration_failed", str(exc))
    else:
        errors.add(
            "incomplete_range",
            "cannot enumerate the canonical upstream range without a validated base and fetched refs",
        )

    ledger["range"] = {
        "mode": range_mode,
        "base_ref": range_base,
        "fork_ref": fork_ref,
        "upstream_ref": upstream_ref,
        "commit_shas": range_shas,
        "commit_count": len(range_shas),
        "sha256": hashlib.sha256("\n".join(range_shas).encode("ascii")).hexdigest(),
        "complete": histories_complete and bool(range_base) and len(blocks) == len(range_shas) and args.max_commits == 0,
    }
    if not ledger["range"]["complete"]:
        errors.add("incomplete_range", "the complete canonical upstream range could not be proven")

    overrides_path = output_path(repo_root, args.overrides).resolve()
    overrides = load_overrides(overrides_path, errors)
    divergence_registry_path = output_path(
        repo_root, args.intentional_divergence
    ).resolve()
    approved_divergence_prefixes = load_approved_divergence_prefixes(
        divergence_registry_path, errors
    )
    local_commits: set[str] = set()
    local_tree_map: dict[str, set[str]] = {}
    local_patch_map: dict[str, set[str]] = {}
    upstream_patch_map: dict[str, set[str]] = {}
    if histories_complete and range_base and ref_sha(repo_root, fork_ref) and ref_sha(repo_root, upstream_ref):
        try:
            local_commits, local_tree_map = local_commit_and_tree_maps(repo_root, fork_ref)
            local_patch_map = patch_id_map(repo_root, [fork_ref])
            upstream_patch_map = patch_id_map(repo_root, [upstream_ref, f"^{range_base}"])
        except TruthError as exc:
            errors.add("equivalence_enumeration_failed", str(exc))
    elif blocks:
        errors.add(
            "equivalence_unavailable",
            "exact SHA, patch-id, and tree-content checks require complete fetched histories and a canonical base",
        )

    rows = classify_blocks(
        repo_root,
        blocks,
        fork_ref,
        local_commits,
        local_tree_map,
        local_patch_map,
        upstream_patch_map,
        overrides,
        approved_divergence_prefixes,
        errors,
    )
    unowned_rows = [row for row in rows if row["disposition"] == "pending"]
    if unowned_rows:
        errors.add(
            "unowned_divergence",
            f"{len(unowned_rows)} upstream commits have no proven owner or semantic disposition",
        )

    by_ticket: Counter[int] = Counter(int(row["target_ticket"]) for row in rows)
    by_disposition: Counter[str] = Counter(str(row["disposition"]) for row in rows)
    classification_records = [
        {
            "sha": row["sha"],
            "disposition": row["disposition"],
            "classification_rule": row["classification_rule"],
            "semantic_equivalence": row["semantic_equivalence"],
        }
        for row in rows
    ]
    ledger["range"]["classifications"] = classification_records
    ledger["range"]["classification_sha256"] = json_digest(
        {"classifications": classification_records}
    )
    ledger["source_errors"] = errors.records()
    ledger["source_error_counts"] = errors.counts()
    ledger["source_valid"] = not errors.has_errors
    ledger_path = output_path(repo_root, args.out_source_ledger).resolve()
    ledger_path.parent.mkdir(parents=True, exist_ok=True)
    ledger_path.write_text(json.dumps(ledger, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    ledger_sha = json_digest(ledger)

    payload = {
        "schema_version": 2,
        "generator": {"name": "generate-upstream-patch-queue.py", "version": GENERATOR_VERSION},
        "generated_at_utc": utc_now(),
        "source_ledger": {
            "path": public_repo_relative_path(ledger_path, repo_root),
            "sha256": ledger_sha,
            "fork_sha": ledger.get("sources", {}).get("fork", {}).get("fetched_sha"),
            "upstream_sha": ledger.get("sources", {}).get("upstream", {}).get("fetched_sha"),
            "range_sha256": ledger["range"]["sha256"],
            "classification_sha256": ledger["range"]["classification_sha256"],
            "local_source_sha256": ledger.get("local_source_sha256"),
            "local_tree_state_digest": ledger.get("local_source", {}).get("tree_state_digest"),
        },
        "summary": {
            "total_commits": len(rows),
            "complete_range_commit_count": len(range_shas),
            "by_target_ticket": {str(ticket): count for ticket, count in sorted(by_ticket.items())},
            "by_disposition": dict(sorted(by_disposition.items())),
            "unowned_divergence_count": len(unowned_rows),
            "overrides_path": public_repo_relative_path(overrides_path, repo_root),
        },
        "truth": {
            "pass": not errors.has_errors,
            "error_count": sum(errors.counts().values()),
            "errors": errors.records(),
            "error_counts": errors.counts(),
        },
        "commits": rows,
    }
    out_json = output_path(repo_root, args.out_json).resolve()
    out_md = output_path(repo_root, args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    out_md.write_text(render_markdown(payload), encoding="utf-8")
    pre_post_error_counts = errors.counts()
    post_write_local_source, post_write_errors = build_local_source_record(
        repo_root,
        reviewed_dirty_source=args.reviewed_dirty_source,
    )
    if post_write_errors:
        errors.add(
            "local_source_resample_failed",
            "local source could not be resampled after writing queue evidence",
        )
    if json_digest(post_write_local_source) != ledger.get("local_source_sha256"):
        errors.add(
            "local_source_changed_during_generation",
            "local source changed while queue evidence was generated",
        )
    if errors.counts() != pre_post_error_counts:
        ledger["source_errors"] = errors.records()
        ledger["source_error_counts"] = errors.counts()
        ledger["source_valid"] = not errors.has_errors
        ledger_path.write_text(json.dumps(ledger, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        payload["source_ledger"]["sha256"] = json_digest(ledger)
        payload["truth"] = {
            "pass": not errors.has_errors,
            "error_count": sum(errors.counts().values()),
            "errors": errors.records(),
            "error_counts": errors.counts(),
        }
        out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        out_md.write_text(render_markdown(payload), encoding="utf-8")
    print(f"Wrote {ledger_path}")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    print(f"upstream_parity_truth={'PASS' if payload['truth']['pass'] else 'FAIL'}")
    return 0 if payload["truth"]["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
