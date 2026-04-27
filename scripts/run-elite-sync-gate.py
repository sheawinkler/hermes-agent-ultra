#!/usr/bin/env python3
"""Run consolidated ELITE sync gates and emit a single pass/fail report."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import shlex
import subprocess
import sys
import time
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--redteam-cmd",
        default="python3 scripts/run-redteam-gate.py --max-severity-allowed none",
        help="Red-team gate command",
    )
    parser.add_argument(
        "--chaos-cmd",
        default="python3 scripts/run-adapter-chaos-harness.py",
        help="Adapter chaos harness command",
    )
    parser.add_argument(
        "--hotpath-cmd",
        default="python3 scripts/run-zero-copy-hotpath-bench.py",
        help="Zero-copy hot path benchmark command",
    )
    parser.add_argument(
        "--chaos-baseline",
        default="",
        help="Optional baseline adapter chaos report for regression comparison",
    )
    parser.add_argument(
        "--chaos-compare-cmd",
        default="python3 scripts/compare-adapter-chaos-reports.py",
        help="Chaos compare command",
    )
    parser.add_argument("--report-path", default="", help="Optional explicit report path")
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"elite-sync-gate-{stamp}.json"


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
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def extract_report_path(output: str) -> str | None:
    patterns = [
        r"\[redteam-gate\] Report:\s*(.+)",
        r"report=(\S+)",
        r"Report:\s*(\S+)",
    ]
    for line in output.splitlines():
        for pattern in patterns:
            m = re.search(pattern, line)
            if m:
                return m.group(1).strip()
    return None


def slim(raw: dict[str, Any]) -> dict[str, Any]:
    return {
        "command": raw.get("command"),
        "exit_code": raw.get("exit_code"),
        "ok": bool(raw.get("ok")),
        "elapsed_ms": raw.get("elapsed_ms"),
        "stdout_tail": (raw.get("stdout") or "")[-4000:],
        "stderr_tail": (raw.get("stderr") or "")[-4000:],
        "report_path": extract_report_path((raw.get("stdout") or "") + "\n" + (raw.get("stderr") or "")),
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

    redteam_raw = run_shell(args.redteam_cmd, repo_root)
    chaos_raw = run_shell(args.chaos_cmd, repo_root)
    hotpath_raw = run_shell(args.hotpath_cmd, repo_root)

    redteam = slim(redteam_raw)
    chaos = slim(chaos_raw)
    hotpath = slim(hotpath_raw)
    sections: dict[str, Any] = {
        "redteam": redteam,
        "chaos": chaos,
        "hotpath": hotpath,
    }

    compare: dict[str, Any] | None = None
    if args.chaos_baseline.strip():
        baseline = pathlib.Path(args.chaos_baseline).expanduser().resolve()
        current = chaos.get("report_path")
        if baseline.exists() and current:
            compare_output = report_path.with_name(report_path.stem + "-chaos-compare.json")
            compare_cmd = (
                f"{args.chaos_compare_cmd} --baseline {shlex.quote(str(baseline))} "
                f"--current {shlex.quote(str(current))} --output {shlex.quote(str(compare_output))} --json"
            )
            compare_raw = run_shell(compare_cmd, repo_root)
            compare = slim(compare_raw)
            compare["report_path"] = str(compare_output)
            sections["chaos_compare"] = compare
        else:
            sections["chaos_compare"] = {
                "ok": False,
                "reason": "baseline report missing or current chaos report unavailable",
            }

    gate_ok = all(bool(section.get("ok")) for section in sections.values())
    report = {
        "generated_at": dt.datetime.now(dt.UTC).isoformat(),
        "repo_root": str(repo_root),
        "ok": gate_ok,
        "summary": {
            "total_sections": len(sections),
            "passed_sections": sum(1 for section in sections.values() if section.get("ok")),
            "failed_sections": sum(1 for section in sections.values() if not section.get("ok")),
        },
        "sections": sections,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if report["ok"] else "FAILED"
        print(
            f"[elite-sync-gate] {status} "
            f"(passed={report['summary']['passed_sections']}/{report['summary']['total_sections']})"
        )
        print(f"[elite-sync-gate] Report: {report_path}")

    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
