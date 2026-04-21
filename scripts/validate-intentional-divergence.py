#!/usr/bin/env python3
"""Validate intentional-divergence registry quality and review freshness."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
from pathlib import Path
from typing import Any

ALLOWED_STATUS = {"approved", "temporary", "pending_review", "deprecated"}
REQUIRED_FIELDS = (
    "id",
    "status",
    "workstream",
    "summary",
    "owner",
    "ticket",
    "last_reviewed",
    "review_date",
    "rationale",
    "path_prefixes",
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


def ls_tree_paths(repo_root: Path, ref: str) -> set[str]:
    out = run_git(repo_root, ["ls-tree", "-r", "--name-only", ref], check=False)
    return {line.strip() for line in out.splitlines() if line.strip()}


def parse_iso_date(value: str) -> dt.date:
    return dt.date.fromisoformat(value)


def validate_item(
    item: dict[str, Any], *, all_paths: set[str], now: dt.date
) -> dict[str, Any]:
    errors: list[str] = []
    warnings: list[str] = []

    for field in REQUIRED_FIELDS:
        if field not in item:
            errors.append(f"missing field: {field}")

    status = str(item.get("status", ""))
    if status and status not in ALLOWED_STATUS:
        errors.append(f"invalid status: {status}")

    owner = str(item.get("owner", "")).strip()
    if not owner:
        errors.append("owner must be non-empty")

    ticket = item.get("ticket")
    if not isinstance(ticket, int) or ticket <= 0:
        errors.append("ticket must be a positive integer")

    review_date_raw = str(item.get("review_date", "")).strip()
    review_date: dt.date | None = None
    if review_date_raw:
        try:
            review_date = parse_iso_date(review_date_raw)
        except ValueError:
            errors.append("review_date must be ISO-8601 date (YYYY-MM-DD)")
    else:
        errors.append("review_date must be non-empty")

    last_reviewed_raw = str(item.get("last_reviewed", "")).strip()
    if last_reviewed_raw:
        try:
            parse_iso_date(last_reviewed_raw)
        except ValueError:
            errors.append("last_reviewed must be ISO-8601 date (YYYY-MM-DD)")
    else:
        errors.append("last_reviewed must be non-empty")

    rationale = str(item.get("rationale", "")).strip()
    if len(rationale) < 12:
        errors.append("rationale too short")

    prefixes = item.get("path_prefixes")
    if not isinstance(prefixes, list) or not prefixes:
        errors.append("path_prefixes must be a non-empty list")
        prefixes = []
    norm_prefixes = [str(prefix).rstrip("/") for prefix in prefixes if str(prefix).strip()]

    matched = sorted(
        p
        for p in all_paths
        if any(p == prefix or p.startswith(prefix + "/") for prefix in norm_prefixes)
    )
    if not matched:
        warnings.append("no paths currently matched in local/upstream trees")

    overdue = bool(review_date and review_date < now)
    if overdue:
        warnings.append("review_date is overdue")

    return {
        "id": str(item.get("id", "")),
        "status": status,
        "owner": owner,
        "ticket": ticket,
        "review_date": review_date_raw,
        "last_reviewed": last_reviewed_raw,
        "matched_files": len(matched),
        "matched_sample": matched[:20],
        "errors": errors,
        "warnings": warnings,
        "overdue": overdue,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate intentional divergence registry.")
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument(
        "--divergence-file",
        default="docs/parity/intentional-divergence.json",
        help="Intentional divergence registry JSON path (relative to repo root)",
    )
    parser.add_argument(
        "--upstream-remote",
        default="upstream",
        help="Git remote name for official upstream",
    )
    parser.add_argument(
        "--upstream-url",
        default="https://github.com/NousResearch/hermes-agent.git",
        help="URL used if upstream remote is missing",
    )
    parser.add_argument(
        "--upstream-ref",
        default="upstream/main",
        help="Upstream ref used for path coverage checks",
    )
    parser.add_argument(
        "--no-fetch",
        action="store_true",
        help="Skip fetching upstream ref",
    )
    parser.add_argument(
        "--output-json",
        default="docs/parity/divergence-validation.json",
        help="Validation report JSON path (relative to repo root)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Exit non-zero when errors/warnings violate gate",
    )
    parser.add_argument(
        "--allow-warnings",
        action="store_true",
        help="Do not fail check mode on warnings (errors still fail)",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    div_path = (repo_root / args.divergence_file).resolve()
    out_json = (repo_root / args.output_json).resolve()
    now = dt.date.today()

    ensure_remote(repo_root, args.upstream_remote, args.upstream_url)
    if not args.no_fetch:
        run_git(repo_root, ["fetch", args.upstream_remote, "--prune"])

    local_paths = ls_tree_paths(repo_root, "HEAD")
    upstream_paths = ls_tree_paths(repo_root, args.upstream_ref)
    all_paths = local_paths | upstream_paths

    raw = json.loads(div_path.read_text(encoding="utf-8"))
    items = raw.get("items", [])
    if not isinstance(items, list):
        raise RuntimeError(f"invalid items payload in {div_path}")

    rows = [validate_item(i, all_paths=all_paths, now=now) for i in items]
    errors = sum(len(r["errors"]) for r in rows)
    warnings = sum(len(r["warnings"]) for r in rows)
    unowned = sum(1 for r in rows if not str(r.get("owner", "")).strip())
    overdue = sum(1 for r in rows if bool(r.get("overdue")))

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "divergence_file": str(div_path),
        "summary": {
            "items": len(rows),
            "errors": errors,
            "warnings": warnings,
            "unowned": unowned,
            "review_overdue": overdue,
            "local_paths": len(local_paths),
            "upstream_paths": len(upstream_paths),
        },
        "items": rows,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    print(f"Wrote {out_json}")
    print(
        "Summary:",
        json.dumps(payload["summary"], sort_keys=True),
    )

    if args.check:
        has_errors = errors > 0
        has_warnings = warnings > 0 and not args.allow_warnings
        if has_errors or has_warnings:
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
