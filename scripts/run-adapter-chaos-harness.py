#!/usr/bin/env python3
"""Run deterministic adapter chaos harness tests and write a JSON report."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import shlex
import subprocess
import sys


DEFAULT_COMMAND = (
    "cargo test -p hermes-agent chaos_harness_profiles_verify_retry_and_fallback -- --nocapture"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--repo-root",
        default=".",
        help="Repository root (default: current directory).",
    )
    parser.add_argument(
        "--command",
        default=DEFAULT_COMMAND,
        help=f"Command to run (default: {DEFAULT_COMMAND!r}).",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Optional explicit output report path.",
    )
    return parser.parse_args()


def default_output_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"adapter-chaos-{stamp}.json"


def extract_failure_excerpt(stdout: str, stderr: str) -> str:
    combined = "\n".join([stdout.strip(), stderr.strip()]).strip()
    marker = "adapter chaos harness mismatches:"
    idx = combined.lower().find(marker)
    if idx >= 0:
        return combined[idx : idx + 4000]
    return combined[-2000:]


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    output_path = (
        pathlib.Path(args.output).expanduser().resolve()
        if args.output
        else default_output_path(repo_root)
    )
    output_path.parent.mkdir(parents=True, exist_ok=True)

    cmd = shlex.split(args.command)
    started = dt.datetime.now(dt.UTC)
    proc = subprocess.run(
        cmd,
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
    )
    finished = dt.datetime.now(dt.UTC)

    report = {
        "timestamp_utc": finished.isoformat(timespec="seconds") + "Z",
        "duration_seconds": round((finished - started).total_seconds(), 3),
        "repo_root": str(repo_root),
        "command": cmd,
        "exit_code": proc.returncode,
        "passed": proc.returncode == 0,
        "stdout_tail": proc.stdout[-8000:],
        "stderr_tail": proc.stderr[-8000:],
    }
    if proc.returncode != 0:
        report["failure_excerpt"] = extract_failure_excerpt(proc.stdout, proc.stderr)

    output_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    summary = (
        f"Adapter chaos harness {'PASSED' if report['passed'] else 'FAILED'} "
        f"(exit={proc.returncode}) report={output_path}"
    )
    print(summary)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
