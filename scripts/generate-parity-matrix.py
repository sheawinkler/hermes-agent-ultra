#!/usr/bin/env python3
"""Generate a parity gap matrix between local and upstream branches."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
from collections import Counter
from pathlib import Path
from typing import Iterable


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
    md.append(f"| Commits behind local (`upstream` only) | {s['commits_behind']} |")
    md.append(f"| Commits ahead local (`local` only) | {s['commits_ahead']} |")
    md.append(f"| Files missing from local (`local..upstream`) | {s['files_missing_from_local']} |")
    md.append(f"| Files unique to local (`upstream..local`) | {s['files_unique_to_local']} |")
    md.append(f"| Total files changed (`local...upstream`) | {s['tree_files_changed']} |")
    md.append(f"| Insertions (`local...upstream`) | {s['tree_insertions']} |")
    md.append(f"| Deletions (`local...upstream`) | {s['tree_deletions']} |")
    md.append("")
    md.append(f"## Top {top_n} missing-from-local buckets")
    md.append("")
    md.append("| Bucket | Files |")
    md.append("| --- | ---: |")
    for row in data["top_missing_from_local"]:
        md.append(f"| `{row['path']}` | {row['count']} |")
    if not data["top_missing_from_local"]:
        md.append("| _(none)_ | 0 |")
    md.append("")
    md.append(f"## Top {top_n} local-only buckets")
    md.append("")
    md.append("| Bucket | Files |")
    md.append("| --- | ---: |")
    for row in data["top_unique_to_local"]:
        md.append(f"| `{row['path']}` | {row['count']} |")
    if not data["top_unique_to_local"]:
        md.append("| _(none)_ | 0 |")
    md.append("")
    md.append("## Notes")
    md.append("")
    md.append("- Data is computed directly from git refs in this repository.")
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

    missing_from_local_files = run_git(repo_root, ["diff", "--name-only", f"{local_ref}..{upstream_ref}"]).splitlines()
    unique_to_local_files = run_git(repo_root, ["diff", "--name-only", f"{upstream_ref}..{local_ref}"]).splitlines()

    shortstat_raw = run_git(repo_root, ["diff", "--shortstat", f"{local_ref}...{upstream_ref}"], check=False)
    if not shortstat_raw:
        shortstat_raw = run_git(repo_root, ["diff", "--shortstat", local_ref, upstream_ref], check=False)
    shortstat_all = parse_shortstat(shortstat_raw)

    missing_counter = bucket(missing_from_local_files)
    unique_counter = bucket(unique_to_local_files)

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
            "files_missing_from_local": len([p for p in missing_from_local_files if p]),
            "files_unique_to_local": len([p for p in unique_to_local_files if p]),
            "tree_files_changed": shortstat_all["files_changed"],
            "tree_insertions": shortstat_all["insertions"],
            "tree_deletions": shortstat_all["deletions"],
        },
        "top_missing_from_local": top(missing_counter, args.top_n),
        "top_unique_to_local": top(unique_counter, args.top_n),
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
