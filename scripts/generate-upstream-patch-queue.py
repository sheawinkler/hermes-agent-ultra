#!/usr/bin/env python3
"""Generate auditable upstream missing patch queue for parity backfill."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path
from typing import Any

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
PYTHON_TEST_SURFACE_PREFIXES: tuple[str, ...] = (
    "tests/",
    "test/",
)
RUST_PRIMARY_SUPERSEDE_PREFIXES: tuple[str, ...] = (
    "tests/",
    "test/",
    "acp_adapter/",
    "agent/",
    "batch_runner.py",
    "hermes_cli/",
    "hermes_state.py",
    "hermes_constants.py",
    "hermes_logging.py",
    "mini_swe_runner.py",
    "minisweagent_path.py",
    "mixture_of_agents_tool.py",
    "model_tools.py",
    "gateway/",
    "rl_cli.py",
    "tools/",
    "cron/",
    "providers/",
    "terminal_tool.py",
    "tui_gateway.py",
    "run_agent.py",
    "cli.py",
    "utils.py",
    "web_tools.py",
    "pyproject.toml",
    "uv.lock",
    "requirements.txt",
    "requirements-dev.txt",
    "mypy.ini",
    "ruff.toml",
    "scripts/",
    "locales/",
    "nix/",
    ".github/",
    "ui-tui/",
    "web/",
    "website/",
)
RUST_PRIMARY_RETAINED_PREFIXES: tuple[str, ...] = (
    "skills/",
    "optional-skills/",
    "plugins/",
    "docs/",
    "packaging/",
    "scripts/install.sh",
    "README.md",
    "Dockerfile",
    "flake.nix",
    ".github/workflows/",
)
RUST_PRIMARY_NEUTRAL_PREFIXES: tuple[str, ...] = (
    ".dockerignore",
    ".env.example",
    ".envrc",
    ".gitattributes",
    ".gitignore",
    ".cursorrules",
    "__pycache__/",
    "assets/",
    "configs/",
    "datagen-config-examples/",
    "docs/agents.md",
    "docs/llm_client.md",
    "docs/message_graph.md",
    "docs/tools.md",
    "package-lock.json",
    "package.json",
    "requirements.txt",
    "requirements-dev.txt",
    "setup-hermes.sh",
    "test_run.sh",
    "toolset_distributions.py",
    "TODO.md",
    "VISION.md",
)
DEFAULT_SUBJECT_SUPERSEDE_PATTERNS: tuple[tuple[str, str], ...] = (
    (
        r"^chore\(release\): map .+ in AUTHOR_MAP$",
        "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
    ),
    (
        r"^chore\(release\): add .+ to AUTHOR_MAP",
        "release metadata-only AUTHOR_MAP update from upstream Python pipeline",
    ),
    (
        r"^review\(.+\): .+$",
        "review-only upstream nit commit; no Rust-runtime behavior delta",
    ),
)


def run_git(repo_root: Path, args: list[str], check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args], cwd=repo_root, text=True, capture_output=True
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout.strip()


def ensure_remote(repo_root: Path, remote: str, url: str) -> None:
    remotes = set(run_git(repo_root, ["remote"], check=False).splitlines())
    if remote in remotes:
        return
    run_git(repo_root, ["remote", "add", remote, url])


def fetch_remote_branch(repo_root: Path, remote: str, branch: str) -> None:
    remote_ref = f"{remote}/{branch}"
    refspec = f"refs/heads/{branch}:refs/remotes/{remote}/{branch}"
    proc = subprocess.run(
        ["git", "fetch", "--no-tags", remote, refspec],
        cwd=repo_root,
        text=True,
        capture_output=True,
    )
    if proc.returncode == 0:
        return
    if run_git(repo_root, ["rev-parse", "--verify", remote_ref], check=False):
        print(
            f"warning: scoped fetch for {remote_ref} failed; using existing ref: "
            f"{proc.stderr.strip()}",
            file=sys.stderr,
        )
        return
    raise RuntimeError(f"git fetch {remote} {branch} failed: {proc.stderr.strip()}")


def repo_relative_or_absolute(path: Path, repo_root: Path) -> str:
    try:
        return path.relative_to(repo_root).as_posix()
    except ValueError:
        return str(path)


def classify_ticket(files: list[str]) -> int:
    votes: Counter[int] = Counter()
    for f in files:
        for prefix, ticket in PATH_TO_TICKET:
            if f == prefix or f.startswith(prefix):
                votes[ticket] += 1
                break
    if not votes:
        return 26
    return votes.most_common(1)[0][0]


def is_python_test_surface(path: str) -> bool:
    normalized = path.strip()
    if normalized.startswith("./"):
        normalized = normalized[2:]
    return any(normalized.startswith(prefix) for prefix in PYTHON_TEST_SURFACE_PREFIXES)


def commit_is_python_test_only(files: list[str]) -> bool:
    if not files:
        return False
    return all(is_python_test_surface(path) for path in files)


def _path_matches_any(path: str, prefixes: tuple[str, ...]) -> bool:
    normalized = path.strip()
    if normalized.startswith("./"):
        normalized = normalized[2:]
    for prefix in prefixes:
        if normalized == prefix or normalized.startswith(prefix):
            return True
    return False


def commit_is_rust_primary_superseded(files: list[str]) -> bool:
    if not files:
        return False
    any_superseded_surface = False
    for path in files:
        if _path_matches_any(path, RUST_PRIMARY_NEUTRAL_PREFIXES):
            any_superseded_surface = True
            continue
        if _path_matches_any(path, RUST_PRIMARY_RETAINED_PREFIXES):
            return False
        if _path_matches_any(path, RUST_PRIMARY_SUPERSEDE_PREFIXES):
            any_superseded_surface = True
            continue
        # Any unclassified path is treated as potentially actionable.
        return False
    return any_superseded_surface


def parse_log_blocks(raw: str) -> list[dict[str, Any]]:
    blocks: list[dict[str, Any]] = []
    current_sha = ""
    current_subject = ""
    current_files: list[str] = []

    for line in raw.splitlines():
        if line.startswith("__H__"):
            if current_sha:
                blocks.append(
                    {"sha": current_sha, "subject": current_subject, "files": current_files}
                )
            payload = line[len("__H__") :]
            parts = payload.split("\t", 1)
            current_sha = parts[0].strip()
            current_subject = parts[1].strip() if len(parts) > 1 else ""
            current_files = []
            continue
        value = line.strip()
        if value:
            current_files.append(value)
    if current_sha:
        blocks.append({"sha": current_sha, "subject": current_subject, "files": current_files})
    return blocks


def patch_equivalent_commits(repo_root: Path, local_ref: str, upstream_ref: str) -> set[str]:
    proc = subprocess.run(
        ["git", "cherry", local_ref, upstream_ref],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return set()
    out: set[str] = set()
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 2:
            continue
        marker, sha = parts
        if marker == "-":
            out.add(sha.strip())
    return out


def load_queue_payload(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}


def repo_relative_path(repo_root: Path, path: Path) -> str | None:
    try:
        return path.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        return None


def load_queue_payload_from_ref(repo_root: Path, ref: str, path: Path) -> dict[str, Any]:
    relative = repo_relative_path(repo_root, path)
    if not relative:
        return {}
    proc = subprocess.run(
        ["git", "show", f"{ref}:{relative}"],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return {}
    try:
        return json.loads(proc.stdout)
    except Exception:
        return {}


def load_existing_state(payload: dict[str, Any]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for item in payload.get("commits", []):
        sha = str(item.get("sha", "")).strip()
        if not sha:
            continue
        out[sha] = {
            "disposition": str(item.get("disposition", "pending")),
            "notes": str(item.get("notes", "")),
            "owner": str(item.get("owner", "")),
        }
    return out


def first_prior_sha(payload: dict[str, Any]) -> str:
    commits = payload.get("commits", [])
    if not isinstance(commits, list) or not commits:
        return ""
    first = commits[0]
    if not isinstance(first, dict):
        return ""
    return str(first.get("sha", "")).strip()


def has_merge_base(repo_root: Path, left_ref: str, right_ref: str) -> bool:
    proc = subprocess.run(
        ["git", "merge-base", left_ref, right_ref],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    return proc.returncode == 0 and bool(proc.stdout.strip())


def filter_blocks_from_baseline(
    blocks: list[dict[str, Any]],
    baseline_sha: str,
) -> tuple[list[dict[str, Any]], int]:
    if not baseline_sha:
        return blocks, 0
    for index, block in enumerate(blocks):
        if str(block.get("sha", "")) == baseline_sha:
            return blocks[index:], index
    return blocks, 0


def git_blob_bytes(repo_root: Path, ref: str, path: str) -> bytes | None:
    proc = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=repo_root,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout


def file_identical_between_refs(repo_root: Path, local_ref: str, upstream_ref: str, path: str) -> bool:
    local_blob = git_blob_bytes(repo_root, local_ref, path)
    upstream_blob = git_blob_bytes(repo_root, upstream_ref, path)
    if local_blob is None or upstream_blob is None:
        return local_blob is None and upstream_blob is None
    return local_blob == upstream_blob


def commit_mirrored_between_refs(
    repo_root: Path,
    local_ref: str,
    upstream_ref: str,
    files: list[str],
) -> bool:
    if not files:
        return False
    return all(file_identical_between_refs(repo_root, local_ref, upstream_ref, path) for path in files)


def load_overrides(path: Path) -> tuple[dict[str, dict[str, str]], list[dict[str, str]]]:
    if not path.exists():
        return {}, []
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}, []
    sha_overrides_raw = payload.get("sha", {}) if isinstance(payload, dict) else {}
    subject_patterns_raw = payload.get("subject_patterns", {}) if isinstance(payload, dict) else {}
    sha_overrides: dict[str, dict[str, str]] = {}
    if isinstance(sha_overrides_raw, dict):
        for sha, row in sha_overrides_raw.items():
            if not isinstance(row, dict):
                continue
            sha_overrides[str(sha).strip()] = {
                "disposition": str(row.get("disposition", "")).strip(),
                "notes": str(row.get("notes", "")).strip(),
                "owner": str(row.get("owner", "")).strip(),
            }
    subject_patterns: list[dict[str, str]] = []
    if isinstance(subject_patterns_raw, list):
        for row in subject_patterns_raw:
            if not isinstance(row, dict):
                continue
            pattern = str(row.get("pattern", "")).strip()
            disposition = str(row.get("disposition", "")).strip()
            notes = str(row.get("notes", "")).strip()
            if not pattern or not disposition:
                continue
            subject_patterns.append(
                {
                    "pattern": pattern,
                    "disposition": disposition,
                    "notes": notes,
                }
            )
    return sha_overrides, subject_patterns


def resolve_sha_override(sha: str, overrides: dict[str, dict[str, str]]) -> dict[str, str] | None:
    direct = overrides.get(sha)
    if direct:
        return direct
    for key, value in overrides.items():
        normalized = key.strip()
        if normalized and sha.startswith(normalized):
            return value
    return None


def classify_subject_default(subject: str) -> tuple[str, str] | None:
    text = subject.strip()
    for pattern, note in DEFAULT_SUBJECT_SUPERSEDE_PATTERNS:
        if re.search(pattern, text, flags=re.IGNORECASE):
            return ("superseded", note)
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate upstream missing patch queue.")
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument("--local-ref", default="main")
    parser.add_argument("--upstream-remote", default="upstream")
    parser.add_argument("--upstream-url", default="https://github.com/NousResearch/hermes-agent.git")
    parser.add_argument("--upstream-branch", default="main")
    parser.add_argument("--no-fetch", action="store_true")
    parser.add_argument(
        "--max-commits",
        type=int,
        default=0,
        help="Optional max commits to include (0 means all).",
    )
    parser.add_argument(
        "--out-json",
        default="docs/parity/upstream-missing-queue.json",
        type=Path,
        help="Queue JSON output path (relative to repo root)",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/upstream-missing-queue.md",
        type=Path,
        help="Queue markdown summary path (relative to repo root)",
    )
    parser.add_argument(
        "--prior-ref",
        default=None,
        help=(
            "Git ref to load the prior queue from. Defaults to --local-ref, which "
            "keeps regeneration from preserving dirty/generated worktree state."
        ),
    )
    parser.add_argument(
        "--no-prior-baseline",
        action="store_true",
        help=(
            "Do not use the first prior queued upstream commit as the lower bound "
            "when local and upstream refs have no merge base."
        ),
    )
    parser.add_argument(
        "--allow-python-test-surfaces",
        action="store_true",
        help=(
            "Allow upstream Python test-only commits to remain pending. "
            "Default behavior marks them superseded under Rust-only parity policy."
        ),
    )
    parser.add_argument(
        "--overrides",
        default="docs/parity/queue-overrides.json",
        type=Path,
        help="Optional JSON file with sha and subject-pattern disposition overrides.",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    ensure_remote(repo_root, args.upstream_remote, args.upstream_url)
    if not args.no_fetch:
        fetch_remote_branch(repo_root, args.upstream_remote, args.upstream_branch)

    upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"
    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    overrides_path = (repo_root / args.overrides).resolve()
    prior_ref = args.prior_ref or args.local_ref
    prior_payload = load_queue_payload_from_ref(repo_root, prior_ref, out_json)
    prior_source = prior_ref if prior_payload else "worktree"
    if not prior_payload:
        prior_payload = load_queue_payload(out_json)
    prior = load_existing_state(prior_payload)

    range_expr = f"{args.local_ref}..{upstream_ref}"
    log_raw = run_git(
        repo_root,
        ["log", "--reverse", "--no-merges", "--name-only", "--format=__H__%H%x09%s", range_expr],
        check=False,
    )
    blocks = parse_log_blocks(log_raw)
    skipped_before_baseline = 0
    baseline_start_sha = ""
    if not args.no_prior_baseline and not has_merge_base(repo_root, args.local_ref, upstream_ref):
        baseline_start_sha = first_prior_sha(prior_payload)
        blocks, skipped_before_baseline = filter_blocks_from_baseline(blocks, baseline_start_sha)
    if args.max_commits > 0:
        blocks = blocks[: args.max_commits]

    sha_overrides, subject_pattern_overrides = load_overrides(overrides_path)
    patch_equivalent = patch_equivalent_commits(repo_root, args.local_ref, upstream_ref)

    rows = []
    by_ticket: Counter[int] = Counter()
    by_disposition: Counter[str] = Counter()
    mirrored_cache: dict[tuple[str, str], bool] = {}
    for block in blocks:
        sha = block["sha"]
        files = sorted(set(block["files"]))
        ticket = classify_ticket(files)
        prev = prior.get(sha, {})
        disposition = str(prev.get("disposition", "pending")) or "pending"
        notes = str(prev.get("notes", ""))
        owner = str(prev.get("owner", ""))
        classification_rule = "prior_state" if prev else "default_pending"
        override = resolve_sha_override(sha, sha_overrides)
        if override:
            classification_rule = "sha_override"
            disposition = override.get("disposition") or disposition
            if override.get("notes"):
                notes = override["notes"]
            if override.get("owner"):
                owner = override["owner"]

        if disposition in {"", "pending"} and sha in patch_equivalent:
            disposition = "ported"
            classification_rule = "patch_equivalent_cherry"
            if notes:
                notes += " | "
            notes += f"patch-equivalent commit already present on {args.local_ref}"

        python_test_only = commit_is_python_test_only(files)
        rust_only_superseded = python_test_only and not args.allow_python_test_surfaces
        if rust_only_superseded and disposition in {"", "pending"}:
            disposition = "superseded"
            classification_rule = "rust_only_python_test_guard"
            if notes:
                notes += " | "
            notes += "rust-only parity guard: upstream Python test-only commit not ported"

        rust_primary_superseded = commit_is_rust_primary_superseded(files)
        if rust_primary_superseded and disposition in {"", "pending"}:
            disposition = "superseded"
            classification_rule = "rust_primary_surface_guard"
            if notes:
                notes += " | "
            notes += (
                "rust-primary parity policy: upstream Python runtime surface commit is "
                "covered by Rust-native architecture and tracked via Rust gates"
            )

        if disposition in {"", "pending"}:
            key = (args.local_ref, sha)
            if key not in mirrored_cache:
                mirrored_cache[key] = commit_mirrored_between_refs(
                    repo_root,
                    args.local_ref,
                    upstream_ref,
                    files,
                )
            if mirrored_cache[key]:
                disposition = "mirrored"
                classification_rule = "mirrored_file_state"
                if notes:
                    notes += " | "
                notes += f"all touched files in {upstream_ref} already mirror {args.local_ref}"

        if disposition in {"", "pending"}:
            custom_subject_match = None
            for item in subject_pattern_overrides:
                if re.search(item["pattern"], block["subject"], flags=re.IGNORECASE):
                    custom_subject_match = item
                    break
            if custom_subject_match:
                disposition = custom_subject_match["disposition"]
                classification_rule = "subject_override_pattern"
                if custom_subject_match.get("notes"):
                    if notes:
                        notes += " | "
                    notes += custom_subject_match["notes"]
            else:
                default_subject_match = classify_subject_default(block["subject"])
                if default_subject_match is not None:
                    disposition, match_note = default_subject_match
                    classification_rule = "default_subject_pattern"
                    if match_note:
                        if notes:
                            notes += " | "
                        notes += match_note
        row = {
            "sha": sha,
            "subject": block["subject"],
            "target_ticket": ticket,
            "target_ticket_name": TICKET_NAME.get(ticket, "unknown"),
            "files_touched": len(files),
            "files_sample": files[:20],
            "disposition": disposition,
            "owner": owner,
            "notes": notes,
            "rust_only_guard": {
                "python_test_only_commit": python_test_only,
                "superseded_by_guard": rust_only_superseded,
                "allow_python_test_surfaces": bool(args.allow_python_test_surfaces),
                "rust_primary_surface_only_commit": rust_primary_superseded,
            },
            "classification_rule": classification_rule,
        }
        rows.append(row)
        by_ticket[ticket] += 1
        by_disposition[disposition] += 1

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "refs": {
            "local_ref": args.local_ref,
            "upstream_ref": upstream_ref,
            "range": range_expr,
            "prior_ref": prior_ref,
            "prior_source": prior_source,
            "baseline_start_sha": baseline_start_sha,
            "skipped_before_baseline": skipped_before_baseline,
        },
        "summary": {
            "total_commits": len(rows),
            "by_target_ticket": {str(k): v for k, v in sorted(by_ticket.items())},
            "by_disposition": dict(sorted(by_disposition.items())),
            "overrides_path": repo_relative_or_absolute(overrides_path, repo_root),
            "patch_equivalent_count": len(patch_equivalent),
        },
        "commits": rows,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md: list[str] = []
    md.append("# Upstream Missing Patch Queue")
    md.append("")
    md.append(f"Generated: `{payload['generated_at_utc']}`")
    md.append("")
    md.append(
        f"- Range: `{payload['refs']['range']}`; total commits tracked: `{payload['summary']['total_commits']}`."
    )
    md.append("")
    md.append("| Ticket | Label | Commit Count |")
    md.append("| ---: | --- | ---: |")
    for ticket, count in sorted(by_ticket.items()):
        md.append(f"| #{ticket} | {TICKET_NAME.get(ticket, 'unknown')} | {count} |")
    md.append("")
    md.append("| Disposition | Commit Count |")
    md.append("| --- | ---: |")
    for disposition, count in sorted(by_disposition.items()):
        md.append(f"| {disposition} | {count} |")
    md.append("")
    md.append("## First 100 Pending Commits")
    md.append("")
    md.append("| SHA | Ticket | Subject |")
    md.append("| --- | ---: | --- |")
    pending_count = 0
    for row in rows:
        if row["disposition"] != "pending":
            continue
        subject_escaped = row["subject"].replace("|", "\\|")
        md.append(
            f"| `{row['sha'][:12]}` | #{row['target_ticket']} | {subject_escaped} |"
        )
        pending_count += 1
        if pending_count >= 100:
            break
    if pending_count == 0:
        md.append("| _(none)_ | - | - |")

    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
