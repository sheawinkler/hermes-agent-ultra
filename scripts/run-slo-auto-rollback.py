#!/usr/bin/env python3
"""Run an SLO check command and auto-execute rollback command on failure."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import subprocess
import sys
import time
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--check-cmd",
        required=True,
        help="SLO check command; non-zero means violation",
    )
    parser.add_argument(
        "--rollback-cmd",
        required=True,
        help="Rollback command to execute on violation",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit output report path",
    )
    parser.add_argument("--dry-run", action="store_true", help="Do not run rollback command")
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"slo-auto-rollback-{stamp}.json"


def run_shell(command: str, cwd: pathlib.Path) -> dict[str, Any]:
    started = time.time()
    proc = subprocess.run(
        ["bash", "-lc", command],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        check=False,
    )
    elapsed_ms = int((time.time() - started) * 1000)
    return {
        "command": command,
        "exit_code": proc.returncode,
        "ok": proc.returncode == 0,
        "elapsed_ms": elapsed_ms,
        "stdout_tail": (proc.stdout or "")[-4000:],
        "stderr_tail": (proc.stderr or "")[-4000:],
    }


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    report_path = (
        pathlib.Path(args.report_path).expanduser().resolve()
        if args.report_path
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)

    check = run_shell(args.check_cmd, repo_root)
    violated = not bool(check.get("ok"))
    rollback: dict[str, Any] | None = None
    if violated and not args.dry_run:
        rollback = run_shell(args.rollback_cmd, repo_root)
    elif violated and args.dry_run:
        rollback = {
            "command": args.rollback_cmd,
            "ok": False,
            "skipped": True,
            "reason": "dry_run",
        }

    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "ok": not violated,
        "violated": violated,
        "dry_run": bool(args.dry_run),
        "check": check,
        "rollback": rollback,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        if violated:
            rb = rollback or {}
            rb_status = "executed" if rb.get("ok") else "failed_or_skipped"
            print(f"[slo-auto-rollback] VIOLATED (rollback={rb_status})")
        else:
            print("[slo-auto-rollback] PASSED")
        print(f"[slo-auto-rollback] Report: {report_path}")

    return 0 if not violated else 1


if __name__ == "__main__":
    sys.exit(main())
