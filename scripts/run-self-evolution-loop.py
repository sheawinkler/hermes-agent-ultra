#!/usr/bin/env python3
"""Run an integrated self-evolution loop and emit actionable recommendations."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import subprocess
import sys
import time
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--objective",
        default="",
        help="Optional objective text included in the loop report",
    )
    parser.add_argument(
        "--golden-cmd",
        default="python3 scripts/run-golden-parity-harness.py --auto-issue",
        help="Golden parity harness command",
    )
    parser.add_argument(
        "--eval-cmd",
        default="python3 scripts/run-eval-trend-gate.py --allow-missing-baseline --json",
        help="Eval trend gate command",
    )
    parser.add_argument(
        "--elite-cmd",
        default="python3 scripts/run-elite-sync-gate.py --json",
        help="Consolidated elite gate command",
    )
    parser.add_argument(
        "--skip-golden",
        action="store_true",
        help="Skip golden parity harness run",
    )
    parser.add_argument(
        "--skip-eval",
        action="store_true",
        help="Skip eval trend gate run",
    )
    parser.add_argument(
        "--skip-elite",
        action="store_true",
        help="Skip elite sync gate run",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit report path",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"self-evolution-loop-{stamp}.json"


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
        r"\[[^\]]+\]\s*Report:\s*(.+)",
        r"report=(\S+)",
        r'"report_path"\s*:\s*"([^"]+)"',
    ]
    for line in output.splitlines():
        for pattern in patterns:
            match = re.search(pattern, line)
            if match:
                return match.group(1).strip()
    return None


def read_json(path: pathlib.Path) -> dict[str, Any] | None:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return None
    return raw if isinstance(raw, dict) else None


def slim(raw: dict[str, Any], repo_root: pathlib.Path) -> dict[str, Any]:
    output = (raw.get("stdout") or "") + "\n" + (raw.get("stderr") or "")
    report_path_raw = extract_report_path(output)
    report_path = ""
    report_payload: dict[str, Any] | None = None
    if report_path_raw:
        candidate = pathlib.Path(report_path_raw).expanduser()
        if not candidate.is_absolute():
            candidate = (repo_root / candidate).resolve()
        report_path = str(candidate)
        if candidate.exists():
            report_payload = read_json(candidate)
    return {
        "command": raw.get("command"),
        "exit_code": raw.get("exit_code"),
        "ok": bool(raw.get("ok")),
        "elapsed_ms": raw.get("elapsed_ms"),
        "stdout_tail": (raw.get("stdout") or "")[-4000:],
        "stderr_tail": (raw.get("stderr") or "")[-4000:],
        "report_path": report_path,
        "report_ok": (
            report_payload.get("ok")
            if isinstance(report_payload, dict)
            and isinstance(report_payload.get("ok"), bool)
            else None
        ),
        "report_excerpt": (
            {
                key: report_payload.get(key)
                for key in (
                    "summary",
                    "missing_commands",
                    "missing_tui_tests",
                    "drift",
                    "sections",
                )
                if report_payload and key in report_payload
            }
            if isinstance(report_payload, dict)
            else None
        ),
    }


def recommendation(
    rec_id: str,
    severity: str,
    title: str,
    reason: str,
    command: str,
) -> dict[str, str]:
    return {
        "id": rec_id,
        "severity": severity,
        "title": title,
        "reason": reason,
        "command": command,
    }


def build_recommendations(
    objective: str,
    sections: dict[str, dict[str, Any]],
) -> list[dict[str, str]]:
    out: list[dict[str, str]] = []
    objective_hint = (
        f" Objective: {objective.strip()}."
        if objective and objective.strip()
        else ""
    )

    golden = sections.get("golden_parity")
    if golden and not golden.get("ok", False):
        out.append(
            recommendation(
                "PARITY_DRIFT",
                "P0",
                "Resolve command/TUI parity drift before feature work",
                "Golden parity harness failed; upstream command surface or required TUI contracts drifted."
                + objective_hint,
                "python3 scripts/run-golden-parity-harness.py --auto-issue",
            )
        )

    eval_trend = sections.get("eval_trend")
    if eval_trend and not eval_trend.get("ok", False):
        out.append(
            recommendation(
                "EVAL_REGRESSION",
                "P0",
                "Recover eval trend before promotion",
                "Eval trend gate failed; current behavior regressed against baseline."
                + objective_hint,
                "hermes route-autotune plan --apply --json && python3 scripts/run-eval-trend-gate.py --json",
            )
        )

    elite = sections.get("elite_sync")
    if elite and not elite.get("ok", False):
        out.append(
            recommendation(
                "ELITE_GATE_FAIL",
                "P0",
                "Hold release and remediate elite gate failures",
                "Consolidated elite gate failed; one or more hardening/performance/parity sections failed."
                + objective_hint,
                "python3 scripts/run-elite-sync-gate.py --json",
            )
        )

    if not out:
        out.append(
            recommendation(
                "PROMOTE_BASELINE",
                "P2",
                "Promote current state as next baseline",
                "All enabled sections passed; safe to store this run as a quality baseline."
                + objective_hint,
                "python3 scripts/generate-global-parity-proof.py --repo-root .",
            )
        )
    return out


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

    sections: dict[str, dict[str, Any]] = {}
    if not args.skip_golden:
        sections["golden_parity"] = slim(run_shell(args.golden_cmd, repo_root), repo_root)
    if not args.skip_eval:
        sections["eval_trend"] = slim(run_shell(args.eval_cmd, repo_root), repo_root)
    if not args.skip_elite:
        sections["elite_sync"] = slim(run_shell(args.elite_cmd, repo_root), repo_root)

    total = len(sections)
    passed = sum(1 for section in sections.values() if section.get("ok"))
    ok = all(section.get("ok") for section in sections.values()) if sections else True
    recommendations = build_recommendations(args.objective, sections)

    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "objective": args.objective.strip(),
        "ok": ok,
        "summary": {
            "total_sections": total,
            "passed_sections": passed,
            "failed_sections": max(total - passed, 0),
            "intelligence_index": round((passed / total) * 100.0, 2) if total else 100.0,
        },
        "sections": sections,
        "recommendations": recommendations,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if report["ok"] else "FAILED"
        summary = report["summary"]
        print(
            f"[self-evolution-loop] {status} "
            f"(passed={summary['passed_sections']}/{summary['total_sections']} "
            f"index={summary['intelligence_index']})"
        )
        print(f"[self-evolution-loop] Report: {report_path}")
        if recommendations:
            print("[self-evolution-loop] Recommendations:")
            for rec in recommendations:
                print(f"- [{rec['severity']}] {rec['title']} :: {rec['command']}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
