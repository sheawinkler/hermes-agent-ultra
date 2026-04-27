#!/usr/bin/env python3
"""Run zero-copy hot-path benchmark tests and emit a JSON report."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import shlex
import subprocess


DEFAULT_COMMAND = (
    "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repo root (default: current dir)")
    parser.add_argument("--command", default=DEFAULT_COMMAND, help="Benchmark command")
    parser.add_argument("--output", default="", help="Optional explicit output file")
    return parser.parse_args()


def default_output(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"zero-copy-hotpath-{stamp}.json"


def parse_ns_per_eval(stdout: str) -> int | None:
    match = re.search(r"tool_policy_hot_path_ns_per_eval=(\d+)", stdout)
    if not match:
        return None
    return int(match.group(1))


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    output = pathlib.Path(args.output).expanduser().resolve() if args.output else default_output(repo_root)
    output.parent.mkdir(parents=True, exist_ok=True)

    started = dt.datetime.now(dt.timezone.utc)
    proc = subprocess.run(
        shlex.split(args.command),
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
    )
    finished = dt.datetime.now(dt.timezone.utc)

    report = {
        "timestamp_utc": finished.isoformat(timespec="seconds"),
        "duration_seconds": round((finished - started).total_seconds(), 3),
        "repo_root": str(repo_root),
        "command": shlex.split(args.command),
        "exit_code": proc.returncode,
        "passed": proc.returncode == 0,
        "ns_per_eval": parse_ns_per_eval(proc.stdout),
        "stdout_tail": proc.stdout[-8000:],
        "stderr_tail": proc.stderr[-8000:],
    }
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(
        f"Zero-copy hot-path bench {'PASSED' if report['passed'] else 'FAILED'} "
        f"(ns_per_eval={report['ns_per_eval']}) report={output}"
    )
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
