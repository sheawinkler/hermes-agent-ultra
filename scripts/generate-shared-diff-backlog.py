#!/usr/bin/env python3
"""Generate a file-level ledger for shared-different upstream parity drift."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
from collections import Counter
from pathlib import Path
from typing import Any


PATH_TO_WORKSTREAM: list[tuple[str, str]] = [
    ("crates/hermes-agent/src/memory_plugins", "WS3"),
    ("crates/hermes-agent/src/runtime", "WS2"),
    ("crates/hermes-agent/src/config", "WS2"),
    ("crates/hermes-agent/src/gateway", "WS2"),
    ("crates/hermes-agent", "WS2"),
    ("crates/hermes-cli", "WS2"),
    ("crates/hermes-config", "WS2"),
    ("crates/hermes-gateway", "WS2"),
    ("crates/hermes-tools", "WS3"),
    ("crates/hermes-platform", "WS3"),
    ("crates/hermes-plugins", "WS3"),
    ("crates/hermes-mcp", "WS3"),
    ("crates/hermes-secrets", "WS7"),
    ("crates/hermes-security", "WS7"),
    ("scripts/upstream_webhook_sync.py", "WS7"),
    ("scripts/run-upstream-webhook", "WS7"),
    ("scripts/install-upstream-webhook", "WS7"),
    ("scripts/setup-upstream-webhook", "WS7"),
    ("scripts/sync-upstream.sh", "WS7"),
    ("scripts/cron-upstream-sync.sh", "WS7"),
    ("scripts/install-upstream-sync-cron.sh", "WS7"),
    ("scripts/upstream-risk-paths.txt", "WS7"),
    ("gateway", "WS2"),
    ("plugins", "WS3"),
    ("tools", "WS3"),
    ("environments", "WS3"),
    ("web", "WS5"),
    ("packaging", "WS8"),
    ("tests", "WS6"),
    (".github/workflows", "WS6"),
    ("skills", "WS4"),
    ("optional-skills", "WS4"),
    ("ui-tui", "WS5"),
    ("website", "WS5"),
]


CLASSIFICATION_TO_STATUS = {
    "branding_only": "cleared_non_runtime",
    "policy_only": "cleared_non_runtime",
    "intentional_divergence": "cleared_intentional_divergence",
    "functional": "pending_review",
}


def run_git(repo_root: Path, args: list[str], check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout.strip()


def ensure_remote(repo_root: Path, remote: str, url: str) -> None:
    remotes = set(run_git(repo_root, ["remote"], check=False).splitlines())
    if remote not in remotes:
        run_git(repo_root, ["remote", "add", remote, url])


def fetch_remote_branch(repo_root: Path, remote: str, branch: str) -> None:
    refspec = f"refs/heads/{branch}:refs/remotes/{remote}/{branch}"
    run_git(repo_root, ["fetch", "--no-tags", remote, refspec])


def parse_ls_tree_blob_line(line: str) -> tuple[str, str] | None:
    if "\t" not in line:
        return None
    lhs, path = line.split("\t", 1)
    parts = lhs.split()
    if len(parts) != 3:
        return None
    _, obj_type, sha = parts
    if obj_type != "blob":
        return None
    return path, sha


def ls_tree_blobs(repo_root: Path, ref: str) -> dict[str, str]:
    out = run_git(repo_root, ["ls-tree", "-r", "--full-tree", ref])
    blobs: dict[str, str] = {}
    for line in out.splitlines():
        parsed = parse_ls_tree_blob_line(line)
        if parsed is not None:
            path, sha = parsed
            blobs[path] = sha
    return blobs


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def classify_workstream(path: str) -> str:
    for prefix, ws in PATH_TO_WORKSTREAM:
        if path == prefix or path.startswith(prefix + "/"):
            return ws
    return "WS8"


def longest_prefix_match(path: str, items: list[dict[str, Any]]) -> dict[str, Any] | None:
    best: dict[str, Any] | None = None
    best_len = -1
    for item in items:
        prefix = str(item.get("path", "")).strip().rstrip("/")
        if not prefix:
            continue
        if path == prefix or path.startswith(prefix + "/"):
            if len(prefix) > best_len:
                best = item
                best_len = len(prefix)
    return best


def rust_requirement_for_path(path: str, classification: str) -> str:
    if classification in {"branding_only", "policy_only", "intentional_divergence"}:
        return "not_required_for_runtime"
    if path.startswith("tests/"):
        return "rust_equivalent_or_contract_test_required"
    if path.startswith(("plugins/", "tools/", "gateway/", "environments/")):
        return "rust_native_runtime_parity_required_if_needed"
    if path.startswith(("ui-tui/", "website/", "web/")):
        return "rust_primary_ux_or_documented_divergence_required"
    if path.startswith(("skills/", "optional-skills/")):
        return "skill_catalog_contract_required_if_needed"
    if path.endswith(".py"):
        return "rust_port_required_if_runtime_surface"
    return "needs_rust_relevance_review"


def action_for(classification: str, rust_requirement: str) -> str:
    if classification in {"branding_only", "policy_only"}:
        return "No runtime implementation; keep rationale current."
    if classification == "intentional_divergence":
        return "No vendoring; verify approved divergence remains current."
    if classification == "functional":
        if rust_requirement == "rust_equivalent_or_contract_test_required":
            return "Map upstream test intent to Rust/Python contract coverage; add missing regression tests."
        if rust_requirement == "rust_primary_ux_or_documented_divergence_required":
            return "Compare UX behavior; port necessary interaction semantics or document stronger local UX."
        if rust_requirement == "skill_catalog_contract_required_if_needed":
            return "Verify skill catalog/runtime loader coverage; add native contract tests if missing."
        return "Inspect upstream/local behavior; implement Rust-native parity only if local coverage is missing."
    return "Add classification, owner, issue, and surgical plan before implementation."


def build_ledger(
    *,
    repo_root: Path,
    local_ref: str,
    upstream_ref: str,
    classification_items: list[dict[str, Any]],
) -> dict[str, Any]:
    local_tree = ls_tree_blobs(repo_root, local_ref)
    upstream_tree = ls_tree_blobs(repo_root, upstream_ref)
    shared_paths = sorted(set(local_tree) & set(upstream_tree))
    shared_different = [
        path for path in shared_paths if local_tree[path] != upstream_tree[path]
    ]

    entries: list[dict[str, Any]] = []
    for path in shared_different:
        matched = longest_prefix_match(path, classification_items)
        classification = (
            str(matched.get("classification", "unclassified")) if matched else "unclassified"
        )
        rust_requirement = rust_requirement_for_path(path, classification)
        status = CLASSIFICATION_TO_STATUS.get(classification, "pending_classification")
        entries.append(
            {
                "path": path,
                "workstream": classify_workstream(path),
                "status": status,
                "classification": classification,
                "classification_path": str(matched.get("path", "")) if matched else "",
                "necessity": "necessary_review"
                if classification == "functional"
                else "not_runtime_required"
                if classification in {"branding_only", "policy_only"}
                else "approved_divergence"
                if classification == "intentional_divergence"
                else "unknown",
                "rust_requirement": rust_requirement,
                "existing_coverage": "claimed_by_shared_diff_classification"
                if matched
                else "unclaimed",
                "owner": str(matched.get("owner", "")) if matched else "",
                "ticket": matched.get("ticket", "") if matched else "",
                "action": action_for(classification, rust_requirement),
                "local_blob": local_tree[path],
                "upstream_blob": upstream_tree[path],
            }
        )

    by_status = Counter(str(entry["status"]) for entry in entries)
    by_classification = Counter(str(entry["classification"]) for entry in entries)
    by_workstream = Counter(str(entry["workstream"]) for entry in entries)
    by_rust_requirement = Counter(str(entry["rust_requirement"]) for entry in entries)

    return {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "refs": {
            "local_ref": local_ref,
            "local_sha": run_git(repo_root, ["rev-parse", local_ref]),
            "upstream_ref": upstream_ref,
            "upstream_sha": run_git(repo_root, ["rev-parse", upstream_ref]),
        },
        "summary": {
            "total_shared_different": len(entries),
            "pending_classification": by_status.get("pending_classification", 0),
            "pending_review": by_status.get("pending_review", 0),
            "cleared_non_runtime": by_status.get("cleared_non_runtime", 0),
            "cleared_intentional_divergence": by_status.get(
                "cleared_intentional_divergence", 0
            ),
            "by_status": dict(sorted(by_status.items())),
            "by_classification": dict(sorted(by_classification.items())),
            "by_workstream": dict(sorted(by_workstream.items())),
            "by_rust_requirement": dict(sorted(by_rust_requirement.items())),
        },
        "entries": entries,
    }


def render_markdown(payload: dict[str, Any]) -> str:
    summary = payload["summary"]
    lines = [
        "# Shared-Different Backlog",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "## Summary",
        "",
        f"- Total shared-different paths: `{summary['total_shared_different']}`",
        f"- Pending classification: `{summary['pending_classification']}`",
        f"- Pending functional review: `{summary['pending_review']}`",
        f"- Cleared non-runtime: `{summary['cleared_non_runtime']}`",
        f"- Cleared intentional divergence: `{summary['cleared_intentional_divergence']}`",
        "",
        "## Status Counts",
        "",
        "| Status | Count |",
        "| --- | ---: |",
    ]
    for key, value in summary["by_status"].items():
        lines.append(f"| `{key}` | {value} |")
    lines.extend(["", "## Workstream Counts", "", "| Workstream | Count |", "| --- | ---: |"])
    for key, value in summary["by_workstream"].items():
        lines.append(f"| `{key}` | {value} |")
    lines.extend(
        [
            "",
            "## Pending Classification",
            "",
            "| Path | Workstream | Action |",
            "| --- | --- | --- |",
        ]
    )
    for entry in payload["entries"]:
        if entry["status"] == "pending_classification":
            lines.append(
                f"| `{entry['path']}` | `{entry['workstream']}` | {entry['action']} |"
            )
    lines.extend(
        [
            "",
            "## Pending Functional Review By Prefix",
            "",
            "| Classification Path | Count |",
            "| --- | ---: |",
        ]
    )
    prefix_counts = Counter(
        str(entry["classification_path"])
        for entry in payload["entries"]
        if entry["status"] == "pending_review"
    )
    for key, value in prefix_counts.most_common():
        lines.append(f"| `{key}` | {value} |")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate a per-file shared-different parity backlog ledger."
    )
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument("--local-ref", default="main")
    parser.add_argument("--upstream-remote", default="upstream")
    parser.add_argument(
        "--upstream-url", default="https://github.com/NousResearch/hermes-agent.git"
    )
    parser.add_argument("--upstream-branch", default="main")
    parser.add_argument("--no-fetch", action="store_true")
    parser.add_argument(
        "--classification",
        default="docs/parity/shared-different-classification.json",
        type=Path,
        help="Shared-different classification registry, relative to repo root.",
    )
    parser.add_argument(
        "--out-json",
        default="docs/parity/shared-diff-backlog.json",
        type=Path,
        help="Output JSON path, relative to repo root.",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/shared-diff-backlog.md",
        type=Path,
        help="Output Markdown path, relative to repo root.",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    ensure_remote(repo_root, args.upstream_remote, args.upstream_url)
    if not args.no_fetch:
        fetch_remote_branch(repo_root, args.upstream_remote, args.upstream_branch)

    upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"
    classification = load_json((repo_root / args.classification).resolve())
    payload = build_ledger(
        repo_root=repo_root,
        local_ref=args.local_ref,
        upstream_ref=upstream_ref,
        classification_items=list(classification.get("items", [])),
    )

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    out_md.write_text(render_markdown(payload), encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    print(json.dumps(payload["summary"], sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
