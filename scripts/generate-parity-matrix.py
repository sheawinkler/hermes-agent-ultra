#!/usr/bin/env python3
"""Generate a parity gap matrix between local and upstream branches.

This script is intentionally robust to unrelated git histories by using:
- tree-level blob hash comparison (path parity independent of merge-base)
- patch-id commit mapping (`git cherry`) for represented vs missing commits
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
from collections import Counter
from pathlib import Path
from typing import Iterable


WORKSTREAM_METADATA: dict[str, dict[str, str | int]] = {
    "WS2": {
        "issue": 6,
        "name": "Core runtime parity",
        "default_risk": "critical",
    },
    "WS3": {
        "issue": 7,
        "name": "Tools and adapters parity",
        "default_risk": "high",
    },
    "WS4": {
        "issue": 8,
        "name": "Skills parity",
        "default_risk": "medium",
    },
    "WS5": {
        "issue": 9,
        "name": "UX parity",
        "default_risk": "medium",
    },
    "WS6": {
        "issue": 10,
        "name": "Tests and CI parity",
        "default_risk": "high",
    },
    "WS7": {
        "issue": 11,
        "name": "Security/secrets/store/webhook parity",
        "default_risk": "critical",
    },
    "WS8": {
        "issue": 12,
        "name": "Compatibility and divergence policy",
        "default_risk": "medium",
    },
}

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
    ("docs", "WS5"),
]

RISK_ORDER = {"low": 0, "medium": 1, "high": 2, "critical": 3}


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


def parse_shortstat(text: str) -> dict[str, int]:
    out = {"files_changed": 0, "insertions": 0, "deletions": 0}
    if not text:
        return out
    files = re.search(r"(\d+)\s+files? changed", text)
    ins = re.search(r"(\d+)\s+insertions?\(\+\)", text)
    dels = re.search(r"(\d+)\s+deletions?\(-\)", text)
    if files:
        out["files_changed"] = int(files.group(1))
    if ins:
        out["insertions"] = int(ins.group(1))
    if dels:
        out["deletions"] = int(dels.group(1))
    return out


def parse_ls_tree_blob_line(line: str) -> tuple[str, str] | None:
    line = line.strip()
    if not line:
        return None
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
        if not parsed:
            continue
        path, sha = parsed
        blobs[path] = sha
    return blobs


def bucket(paths: Iterable[str]) -> Counter[str]:
    c: Counter[str] = Counter()
    for raw in paths:
        path = raw.strip()
        if not path:
            continue
        parts = path.split("/")
        key = "/".join(parts[:2]) if len(parts) > 1 else parts[0]
        c[key] += 1
    return c


def top(counter: Counter[str], n: int) -> list[dict[str, int | str]]:
    return [{"path": k, "count": v} for k, v in counter.most_common(n)]


def classify_workstream(path: str) -> str:
    for prefix, ws in PATH_TO_WORKSTREAM:
        if path == prefix or path.startswith(prefix + "/"):
            return ws
    return "WS8"


def effort_from_count(n: int) -> str:
    if n <= 25:
        return "S"
    if n <= 100:
        return "M"
    if n <= 400:
        return "L"
    return "XL"


def max_risk(a: str, b: str) -> str:
    return a if RISK_ORDER[a] >= RISK_ORDER[b] else b


def maybe_escalate_risk(base_risk: str, gap_count: int) -> str:
    if gap_count >= 1000:
        return "critical"
    if gap_count >= 400:
        return max_risk(base_risk, "high")
    if gap_count >= 120:
        return max_risk(base_risk, "medium")
    return base_risk


def parse_cherry(repo_root: Path, upstream: str, head: str) -> dict[str, object]:
    """
    Parse `git cherry upstream head`.

    - '+' means head commit is not patch-equivalent in upstream.
    - '-' means head commit is patch-equivalent in upstream.
    """
    out = run_git(repo_root, ["cherry", upstream, head], check=False)
    plus: list[str] = []
    minus: list[str] = []
    for raw in out.splitlines():
        raw = raw.strip()
        if not raw:
            continue
        parts = raw.split()
        if len(parts) != 2:
            continue
        sign, sha = parts
        if sign == "+":
            plus.append(sha)
        elif sign == "-":
            minus.append(sha)
    return {
        "plus_count": len(plus),
        "minus_count": len(minus),
        "plus_sample": plus[:20],
        "minus_sample": minus[:20],
    }


def load_intentional_divergence_registry(path: Path) -> list[dict[str, object]]:
    if not path.exists():
        return []
    raw = json.loads(path.read_text(encoding="utf-8"))
    items = raw.get("items", [])
    if not isinstance(items, list):
        return []
    cleaned: list[dict[str, object]] = []
    for item in items:
        if not isinstance(item, dict):
            continue
        prefixes = item.get("path_prefixes", [])
        if not isinstance(prefixes, list):
            prefixes = []
        cleaned.append(
            {
                "id": str(item.get("id", "")),
                "summary": str(item.get("summary", "")),
                "status": str(item.get("status", "")),
                "workstream": str(item.get("workstream", "WS8")),
                "path_prefixes": [str(p) for p in prefixes if p],
            }
        )
    return cleaned


def compute_divergence_coverage(
    items: list[dict[str, object]], candidate_paths: set[str]
) -> tuple[list[dict[str, object]], int]:
    covered_files: set[str] = set()
    coverage_rows: list[dict[str, object]] = []

    for item in items:
        prefixes = item.get("path_prefixes", [])
        if not isinstance(prefixes, list):
            prefixes = []
        matched = sorted(
            p
            for p in candidate_paths
            if any(p == prefix or p.startswith(prefix + "/") for prefix in prefixes)
        )
        covered_files.update(matched)
        coverage_rows.append(
            {
                "id": item["id"],
                "status": item["status"],
                "workstream": item["workstream"],
                "summary": item["summary"],
                "matched_files": len(matched),
                "matched_sample": matched[:20],
            }
        )

    return coverage_rows, len(covered_files)


def render_markdown(data: dict, top_n: int) -> str:
    s = data["summary"]
    refs = data["refs"]
    md: list[str] = []
    md.append("# Parity Matrix")
    md.append("")
    md.append(f"Generated: `{data['generated_at_utc']}`")
    md.append("")
    md.append("## Scope")
    md.append("")
    md.append(f"- Local ref: `{refs['local_ref']}` (`{refs['local_sha']}`)")
    md.append(f"- Upstream ref: `{refs['upstream_ref']}` (`{refs['upstream_sha']}`)")
    md.append(f"- Merge base: `{refs['merge_base'] or 'none (history divergence)'}`")
    md.append("")
    md.append("## Summary")
    md.append("")
    md.append("| Metric | Value |")
    md.append("| --- | ---: |")
    md.append(f"| Commits behind local (`upstream` ancestry only) | {s['commits_behind']} |")
    md.append(f"| Commits ahead local (`local` ancestry only) | {s['commits_ahead']} |")
    md.append(
        f"| Upstream commits missing by patch-id (`git cherry local upstream`, `+`) | {s['upstream_patch_missing']} |"
    )
    md.append(
        f"| Upstream commits represented by patch-id (`git cherry local upstream`, `-`) | {s['upstream_patch_represented']} |"
    )
    md.append(
        f"| Local commits unique by patch-id (`git cherry upstream local`, `+`) | {s['local_patch_unique']} |"
    )
    md.append(f"| Files only in upstream tree | {s['files_only_upstream']} |")
    md.append(f"| Files only in local tree | {s['files_only_local']} |")
    md.append(f"| Shared files identical content | {s['files_shared_identical']} |")
    md.append(f"| Shared files different content | {s['files_shared_different']} |")
    md.append(f"| Total files changed (`local` vs `upstream`) | {s['tree_files_changed']} |")
    md.append(f"| Insertions (`local` vs `upstream`) | {s['tree_insertions']} |")
    md.append(f"| Deletions (`local` vs `upstream`) | {s['tree_deletions']} |")
    md.append("")
    md.append(f"## Top {top_n} upstream-only buckets")
    md.append("")
    md.append("| Bucket | Files |")
    md.append("| --- | ---: |")
    for row in data["top_upstream_only"]:
        md.append(f"| `{row['path']}` | {row['count']} |")
    if not data["top_upstream_only"]:
        md.append("| _(none)_ | 0 |")
    md.append("")
    md.append(f"## Top {top_n} shared-different buckets")
    md.append("")
    md.append("| Bucket | Files |")
    md.append("| --- | ---: |")
    for row in data["top_shared_different"]:
        md.append(f"| `{row['path']}` | {row['count']} |")
    if not data["top_shared_different"]:
        md.append("| _(none)_ | 0 |")
    md.append("")
    md.append(f"## Top {top_n} local-only buckets")
    md.append("")
    md.append("| Bucket | Files |")
    md.append("| --- | ---: |")
    for row in data["top_local_only"]:
        md.append(f"| `{row['path']}` | {row['count']} |")
    if not data["top_local_only"]:
        md.append("| _(none)_ | 0 |")
    md.append("")
    md.append("## Workstream Routing")
    md.append("")
    md.append("| Workstream | Issue | Name | Upstream-only | Shared-different | Risk | Effort |")
    md.append("| --- | ---: | --- | ---: | ---: | --- | --- |")
    for row in data["workstream_matrix"]:
        md.append(
            f"| `{row['workstream']}` | #{row['issue']} | {row['name']} | "
            f"{row['upstream_only']} | {row['shared_different']} | {row['risk']} | {row['effort']} |"
        )
    md.append("")
    md.append("## Commit Mapping")
    md.append("")
    cm = data["commit_mapping"]
    md.append(f"- Upstream missing by patch-id: `{cm['upstream_patch_missing']}`")
    md.append(f"- Upstream represented by patch-id: `{cm['upstream_patch_represented']}`")
    md.append(f"- Local unique by patch-id: `{cm['local_patch_unique']}`")
    md.append(
        f"- Intentional divergence tracked items: `{cm['intentional_divergence_items']}` "
        f"(covered files: `{cm['intentional_divergence_covered_files']}`)"
    )
    if not refs["merge_base"]:
        md.append("- Merge base is absent; patch-id mapping is used as primary commit equivalence signal.")
    md.append("")
    if data["intentional_divergence"]:
        md.append("## Intentional Divergence Registry")
        md.append("")
        md.append("| ID | Status | Workstream | Matched Files | Summary |")
        md.append("| --- | --- | --- | ---: | --- |")
        for item in data["intentional_divergence"]:
            md.append(
                f"| `{item['id']}` | {item['status']} | {item['workstream']} | "
                f"{item['matched_files']} | {item['summary']} |"
            )
        md.append("")
    md.append("")
    md.append("## Notes")
    md.append("")
    md.append("- Data is computed directly from git refs in this repository.")
    md.append("- Tree-level parity classification does not require a merge-base.")
    md.append("- Commit representation/missing uses patch-id equivalence from `git cherry`.")
    md.append("- Default behavior fetches latest upstream refs (`git fetch upstream --prune`).")
    return "\n".join(md) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate parity matrix for local vs upstream.")
    parser.add_argument("--repo-root", default=".", help="Path to repository root.")
    parser.add_argument("--upstream-remote", default="upstream", help="Upstream remote name.")
    parser.add_argument("--local-ref", default="main", help="Local ref to compare.")
    parser.add_argument("--upstream-branch", default="main", help="Upstream branch name.")
    parser.add_argument("--top-n", type=int, default=40, help="Top bucket rows per section.")
    parser.add_argument("--no-fetch", action="store_true", help="Do not fetch upstream before computing.")
    parser.add_argument(
        "--intentional-divergence",
        default="docs/parity/intentional-divergence.json",
        help="Intentional divergence registry (JSON) relative to repo root.",
    )
    parser.add_argument(
        "--output-json",
        default="docs/parity/parity-matrix.json",
        help="Output JSON path relative to repo root.",
    )
    parser.add_argument(
        "--output-md",
        default="docs/parity/parity-matrix.md",
        help="Output Markdown path relative to repo root.",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"
    local_ref = args.local_ref

    if not args.no_fetch:
        run_git(repo_root, ["fetch", args.upstream_remote, "--prune"])

    local_sha = run_git(repo_root, ["rev-parse", local_ref])
    upstream_sha = run_git(repo_root, ["rev-parse", upstream_ref])

    merge_base = run_git(repo_root, ["merge-base", local_ref, upstream_ref], check=False) or None

    behind_ahead = run_git(repo_root, ["rev-list", "--left-right", "--count", f"{upstream_ref}...{local_ref}"])
    parts = behind_ahead.split()
    if len(parts) != 2:
        raise RuntimeError(f"unexpected rev-list output: {behind_ahead!r}")
    commits_behind = int(parts[0])
    commits_ahead = int(parts[1])

    local_tree = ls_tree_blobs(repo_root, local_ref)
    upstream_tree = ls_tree_blobs(repo_root, upstream_ref)

    local_paths = set(local_tree.keys())
    upstream_paths = set(upstream_tree.keys())
    shared_paths = local_paths & upstream_paths
    upstream_only_paths = sorted(upstream_paths - local_paths)
    local_only_paths = sorted(local_paths - upstream_paths)
    shared_identical_paths = sorted(p for p in shared_paths if local_tree[p] == upstream_tree[p])
    shared_different_paths = sorted(p for p in shared_paths if local_tree[p] != upstream_tree[p])

    shortstat_raw = run_git(repo_root, ["diff", "--shortstat", local_ref, upstream_ref], check=False)
    if not shortstat_raw:
        shortstat_raw = run_git(repo_root, ["diff", "--shortstat", f"{local_ref}...{upstream_ref}"], check=False)
    shortstat_all = parse_shortstat(shortstat_raw)

    upstream_only_counter = bucket(upstream_only_paths)
    local_only_counter = bucket(local_only_paths)
    shared_different_counter = bucket(shared_different_paths)

    upstream_vs_local_cherry = parse_cherry(repo_root, local_ref, upstream_ref)
    local_vs_upstream_cherry = parse_cherry(repo_root, upstream_ref, local_ref)

    ws_rollup: dict[str, dict[str, str | int]] = {}
    for ws, meta in WORKSTREAM_METADATA.items():
        ws_rollup[ws] = {
            "workstream": ws,
            "issue": int(meta["issue"]),
            "name": str(meta["name"]),
            "upstream_only": 0,
            "shared_different": 0,
            "local_only": 0,
            "risk": str(meta["default_risk"]),
            "effort": "S",
        }

    for path in upstream_only_paths:
        ws_rollup[classify_workstream(path)]["upstream_only"] += 1
    for path in shared_different_paths:
        ws_rollup[classify_workstream(path)]["shared_different"] += 1
    for path in local_only_paths:
        ws_rollup[classify_workstream(path)]["local_only"] += 1

    for ws, row in ws_rollup.items():
        gap_count = int(row["upstream_only"]) + int(row["shared_different"])
        row["risk"] = maybe_escalate_risk(str(WORKSTREAM_METADATA[ws]["default_risk"]), gap_count)
        row["effort"] = effort_from_count(gap_count)

    divergence_registry_path = (repo_root / args.intentional_divergence).resolve()
    divergence_registry = load_intentional_divergence_registry(divergence_registry_path)
    candidate_divergence_paths = set(local_only_paths) | set(shared_different_paths)
    divergence_coverage, divergence_covered_files = compute_divergence_coverage(
        divergence_registry, candidate_divergence_paths
    )

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "refs": {
            "local_ref": local_ref,
            "local_sha": local_sha,
            "upstream_ref": upstream_ref,
            "upstream_sha": upstream_sha,
            "merge_base": merge_base,
        },
        "summary": {
            "commits_behind": commits_behind,
            "commits_ahead": commits_ahead,
            "upstream_patch_missing": int(upstream_vs_local_cherry["plus_count"]),
            "upstream_patch_represented": int(upstream_vs_local_cherry["minus_count"]),
            "local_patch_unique": int(local_vs_upstream_cherry["plus_count"]),
            "files_only_upstream": len(upstream_only_paths),
            "files_only_local": len(local_only_paths),
            "files_shared_identical": len(shared_identical_paths),
            "files_shared_different": len(shared_different_paths),
            "tree_files_changed": shortstat_all["files_changed"],
            "tree_insertions": shortstat_all["insertions"],
            "tree_deletions": shortstat_all["deletions"],
        },
        "top_upstream_only": top(upstream_only_counter, args.top_n),
        "top_shared_different": top(shared_different_counter, args.top_n),
        "top_local_only": top(local_only_counter, args.top_n),
        # Backward-compatible aliases for older consumers.
        "top_missing_from_local": top(upstream_only_counter, args.top_n),
        "top_unique_to_local": top(local_only_counter, args.top_n),
        "workstream_matrix": sorted(
            ws_rollup.values(),
            key=lambda row: (
                -int(row["upstream_only"]) - int(row["shared_different"]),
                row["workstream"],
            ),
        ),
        "commit_mapping": {
            "upstream_patch_missing": int(upstream_vs_local_cherry["plus_count"]),
            "upstream_patch_represented": int(upstream_vs_local_cherry["minus_count"]),
            "local_patch_unique": int(local_vs_upstream_cherry["plus_count"]),
            "local_patch_equivalent_to_upstream": int(local_vs_upstream_cherry["minus_count"]),
            "upstream_patch_missing_sample": upstream_vs_local_cherry["plus_sample"],
            "upstream_patch_represented_sample": upstream_vs_local_cherry["minus_sample"],
            "local_patch_unique_sample": local_vs_upstream_cherry["plus_sample"],
            "local_patch_equivalent_sample": local_vs_upstream_cherry["minus_sample"],
            "intentional_divergence_items": len(divergence_coverage),
            "intentional_divergence_covered_files": divergence_covered_files,
        },
        "intentional_divergence": divergence_coverage,
    }

    json_path = repo_root / args.output_json
    md_path = repo_root / args.output_md
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)

    json_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    md_path.write_text(render_markdown(payload, args.top_n), encoding="utf-8")

    print(f"Wrote JSON: {json_path}")
    print(f"Wrote Markdown: {md_path}")
    print(f"Commits behind/ahead: {commits_behind}/{commits_ahead}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
