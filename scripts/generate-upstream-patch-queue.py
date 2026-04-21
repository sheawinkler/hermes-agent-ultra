#!/usr/bin/env python3
"""Generate auditable upstream missing patch queue for parity backfill."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
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


def load_existing_state(path: Path) -> dict[str, dict[str, Any]]:
    if not path.exists():
        return {}
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
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
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    ensure_remote(repo_root, args.upstream_remote, args.upstream_url)
    if not args.no_fetch:
        run_git(repo_root, ["fetch", args.upstream_remote, "--prune"])

    upstream_ref = f"{args.upstream_remote}/{args.upstream_branch}"
    range_expr = f"{args.local_ref}..{upstream_ref}"
    log_raw = run_git(
        repo_root,
        ["log", "--reverse", "--no-merges", "--name-only", "--format=__H__%H%x09%s", range_expr],
        check=False,
    )
    blocks = parse_log_blocks(log_raw)
    if args.max_commits > 0:
        blocks = blocks[: args.max_commits]

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    prior = load_existing_state(out_json)

    rows = []
    by_ticket: Counter[int] = Counter()
    by_disposition: Counter[str] = Counter()
    for block in blocks:
        sha = block["sha"]
        files = sorted(set(block["files"]))
        ticket = classify_ticket(files)
        prev = prior.get(sha, {})
        disposition = str(prev.get("disposition", "pending")) or "pending"
        row = {
            "sha": sha,
            "subject": block["subject"],
            "target_ticket": ticket,
            "target_ticket_name": TICKET_NAME.get(ticket, "unknown"),
            "files_touched": len(files),
            "files_sample": files[:20],
            "disposition": disposition,
            "owner": str(prev.get("owner", "")),
            "notes": str(prev.get("notes", "")),
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
        },
        "summary": {
            "total_commits": len(rows),
            "by_target_ticket": {str(k): v for k, v in sorted(by_ticket.items())},
            "by_disposition": dict(sorted(by_disposition.items())),
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
        md.append(
            f"| `{row['sha'][:12]}` | #{row['target_ticket']} | {row['subject'].replace('|', '\\|')} |"
        )
        pending_count += 1
        if pending_count >= 100:
            break
    if pending_count == 0:
        md.append("| _(none)_ | - | - |")
    md.append("")

    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
