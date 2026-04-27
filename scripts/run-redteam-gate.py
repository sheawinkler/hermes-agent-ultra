#!/usr/bin/env python3
"""Deterministic adversarial regression gate for Hermes Agent Ultra."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import shlex
import subprocess
import sys
import time
from typing import Any


SEVERITY_ORDER = {
    "none": -1,
    "info": 0,
    "low": 1,
    "medium": 2,
    "high": 3,
    "critical": 4,
}


def normalize_severity(raw: str | None) -> str:
    value = str(raw or "").strip().lower()
    return value if value in SEVERITY_ORDER else "medium"


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def load_suite(path: pathlib.Path) -> dict[str, Any]:
    try:
        return json.loads(path.read_text())
    except Exception as exc:
        raise SystemExit(f"failed to parse suite file {path}: {exc}") from exc


def run_command(entry: dict[str, Any], repo_root: pathlib.Path) -> dict[str, Any]:
    cmd = entry.get("cmd") or []
    if not isinstance(cmd, list) or not all(isinstance(c, str) for c in cmd):
        return {
            "id": entry.get("id", "unknown"),
            "ok": False,
            "error": "invalid command definition",
        }

    timeout_sec = int(entry.get("timeout_sec", 180))
    severity = normalize_severity(entry.get("severity"))
    started = time.time()
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(repo_root),
            text=True,
            capture_output=True,
            timeout=max(30, timeout_sec),
            check=False,
        )
        elapsed_ms = int((time.time() - started) * 1000)
        stdout = (proc.stdout or "").strip()
        stderr = (proc.stderr or "").strip()
        return {
            "id": entry.get("id", "unknown"),
            "severity": severity,
            "command": " ".join(shlex.quote(c) for c in cmd),
            "exit_code": proc.returncode,
            "ok": proc.returncode == 0,
            "elapsed_ms": elapsed_ms,
            "stdout_tail": stdout.splitlines()[-12:],
            "stderr_tail": stderr.splitlines()[-12:],
        }
    except subprocess.TimeoutExpired as exc:
        elapsed_ms = int((time.time() - started) * 1000)
        return {
            "id": entry.get("id", "unknown"),
            "severity": severity,
            "command": " ".join(shlex.quote(c) for c in cmd),
            "exit_code": 124,
            "ok": False,
            "elapsed_ms": elapsed_ms,
            "error": f"timeout after {exc.timeout}s",
        }


def main() -> int:
    parser = argparse.ArgumentParser(description="Run Hermes adversarial red-team gate")
    parser.add_argument(
        "--repo-root",
        default=str(pathlib.Path(__file__).resolve().parents[1]),
        help="Repository root",
    )
    parser.add_argument(
        "--suite",
        default="scripts/redteam-cases.json",
        help="Suite JSON path (relative to repo root or absolute)",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit report JSON path",
    )
    parser.add_argument(
        "--max-severity-allowed",
        default="none",
        help="Allow failures up to this severity (none|info|low|medium|high|critical).",
    )
    args = parser.parse_args()
    max_allowed = normalize_severity(args.max_severity_allowed)
    max_allowed_rank = SEVERITY_ORDER.get(max_allowed, -1)

    repo_root = pathlib.Path(args.repo_root).resolve()
    suite_path = pathlib.Path(args.suite)
    if not suite_path.is_absolute():
        suite_path = repo_root / suite_path

    suite = load_suite(suite_path)
    commands = suite.get("commands") or []
    if not isinstance(commands, list) or not commands:
        raise SystemExit("suite has no commands")

    results = [run_command(entry, repo_root) for entry in commands]
    passed = sum(1 for r in results if r.get("ok"))
    failed = len(results) - passed
    blocking_failures = [
        result
        for result in results
        if not result.get("ok")
        and SEVERITY_ORDER.get(normalize_severity(result.get("severity")), 2) > max_allowed_rank
    ]
    tolerated_failures = [
        result
        for result in results
        if not result.get("ok")
        and result not in blocking_failures
    ]

    report = {
        "generated_at": utc_now(),
        "suite": suite.get("suite", "redteam"),
        "suite_version": suite.get("version", 1),
        "max_severity_allowed": max_allowed,
        "repo_root": str(repo_root),
        "suite_file": str(suite_path),
        "summary": {
            "total": len(results),
            "passed": passed,
            "failed": failed,
            "failed_blocking": len(blocking_failures),
            "failed_tolerated": len(tolerated_failures),
            "ok": len(blocking_failures) == 0,
        },
        "results": results,
        "blocking_failures": blocking_failures,
        "tolerated_failures": tolerated_failures,
    }

    if args.report_path:
        report_path = pathlib.Path(args.report_path)
        if not report_path.is_absolute():
            report_path = repo_root / report_path
    else:
        out_dir = repo_root / ".sync-reports"
        out_dir.mkdir(parents=True, exist_ok=True)
        report_path = out_dir / f"redteam-gate-{dt.datetime.now(dt.timezone.utc).strftime('%Y%m%d-%H%M%S')}.json"

    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    print(f"[redteam-gate] Report: {report_path}")
    print(
        f"[redteam-gate] Summary: total={len(results)} passed={passed} "
        f"failed={failed} blocking={len(blocking_failures)} "
        f"tolerated={len(tolerated_failures)} max_allowed={max_allowed}"
    )
    return 0 if len(blocking_failures) == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
