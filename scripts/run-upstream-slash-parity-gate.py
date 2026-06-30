#!/usr/bin/env python3
"""Fail when upstream slash-command surface drifts without explicit divergence."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import subprocess
import sys
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument("--local-ref", default="HEAD", help="Local git ref")
    parser.add_argument("--upstream-ref", default="upstream/main", help="Upstream git ref")
    parser.add_argument(
        "--allowlist",
        default="docs/parity/slash-command-divergence.json",
        help="Divergence allowlist JSON path",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit report output path",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def run(cmd: list[str], cwd: pathlib.Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )


def git_ref_exists(repo_root: pathlib.Path, ref: str) -> bool:
    proc = run(["git", "rev-parse", "--verify", ref], repo_root)
    return proc.returncode == 0


def git_show_file(repo_root: pathlib.Path, ref: str, path: str) -> str | None:
    proc = run(["git", "show", f"{ref}:{path}"], repo_root)
    if proc.returncode != 0:
        return None
    return proc.stdout


def git_show_text_surface(repo_root: pathlib.Path, ref: str, paths: list[str]) -> str | None:
    parts: list[str] = []
    for path in paths:
        if path.endswith(".rs"):
            single = git_show_file(repo_root, ref, path)
            if single is not None:
                parts.append(single)
                continue
        listed = run(["git", "ls-tree", "-r", "--name-only", ref, "--", path], repo_root)
        if listed.returncode != 0 or not listed.stdout.strip():
            continue
        for rel in sorted(line for line in listed.stdout.splitlines() if line.endswith(".rs")):
            content = git_show_file(repo_root, ref, rel)
            if content is not None:
                parts.append(content)
    return "\n".join(parts) if parts else None


def extract_upstream_slash_commands(py_source: str) -> list[str]:
    names = sorted(set(re.findall(r'CommandDef\("([a-z0-9_-]+)"', py_source)))
    return [f"/{name}" for name in names]


def extract_local_slash_commands(rs_source: str) -> list[str]:
    start = rs_source.find("pub const SLASH_COMMANDS")
    if start < 0:
        return []
    end = rs_source.find("];", start)
    if end < 0:
        return []
    block = rs_source[start:end]
    return sorted(set(re.findall(r'"(/[^"]+)"', block)))


def load_allowlist(path: pathlib.Path) -> set[str]:
    if not path.exists():
        return set()
    raw = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(raw, dict):
        values = raw.get("allowed_missing_upstream_commands", [])
    elif isinstance(raw, list):
        values = raw
    else:
        values = []
    return {str(v) for v in values if str(v).strip()}


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"upstream-slash-parity-gate-{stamp}.json"


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    allowlist_path = repo_root / args.allowlist
    report_path = (
        pathlib.Path(args.report_path).expanduser().resolve()
        if args.report_path
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)

    refs_ok = git_ref_exists(repo_root, args.local_ref) and git_ref_exists(
        repo_root, args.upstream_ref
    )
    if not refs_ok:
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "ok": False,
            "reason": "missing_git_ref",
            "local_ref": args.local_ref,
            "upstream_ref": args.upstream_ref,
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            print("[upstream-slash-parity] FAILED (missing git ref)")
            print(f"[upstream-slash-parity] Report: {report_path}")
        return 1

    upstream_source = git_show_file(repo_root, args.upstream_ref, "hermes_cli/commands.py")
    local_source = git_show_text_surface(
        repo_root,
        args.local_ref,
        ["crates/hermes-cli/src/commands.rs", "crates/hermes-cli/src/commands"],
    )
    if upstream_source is None or local_source is None:
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "ok": False,
            "reason": "missing_surface_file",
            "local_ref": args.local_ref,
            "upstream_ref": args.upstream_ref,
            "missing_upstream_source": upstream_source is None,
            "missing_local_source": local_source is None,
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            print("[upstream-slash-parity] FAILED (missing source file)")
            print(f"[upstream-slash-parity] Report: {report_path}")
        return 1

    upstream_cmds = set(extract_upstream_slash_commands(upstream_source))
    local_cmds = set(extract_local_slash_commands(local_source))
    allowlisted_missing = load_allowlist(allowlist_path)

    missing_all = sorted(upstream_cmds - local_cmds)
    missing_unallowlisted = sorted(cmd for cmd in missing_all if cmd not in allowlisted_missing)
    extra_local = sorted(local_cmds - upstream_cmds)
    stale_allowlist = sorted(cmd for cmd in allowlisted_missing if cmd in local_cmds)

    ok = not missing_unallowlisted
    report: dict[str, Any] = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "ok": ok,
        "local_ref": args.local_ref,
        "upstream_ref": args.upstream_ref,
        "allowlist_path": str(allowlist_path),
        "counts": {
            "upstream": len(upstream_cmds),
            "local": len(local_cmds),
            "missing_total": len(missing_all),
            "missing_unallowlisted": len(missing_unallowlisted),
            "local_extra": len(extra_local),
            "stale_allowlist_entries": len(stale_allowlist),
        },
        "missing_total": missing_all,
        "missing_unallowlisted": missing_unallowlisted,
        "allowlisted_missing": sorted(cmd for cmd in missing_all if cmd in allowlisted_missing),
        "local_extra": extra_local,
        "stale_allowlist_entries": stale_allowlist,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if ok else "FAILED"
        print(
            "[upstream-slash-parity] "
            f"{status} (missing_unallowlisted={len(missing_unallowlisted)} "
            f"missing_total={len(missing_all)} local_extra={len(extra_local)})"
        )
        print(f"[upstream-slash-parity] Report: {report_path}")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
