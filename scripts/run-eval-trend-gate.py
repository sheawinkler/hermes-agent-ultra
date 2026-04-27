#!/usr/bin/env python3
"""Evaluate benchmark trend regressions and gate on configurable thresholds."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import sys
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--current",
        default="",
        help="Current eval run JSON (defaults to latest in evals/)",
    )
    parser.add_argument(
        "--baseline",
        default="",
        help="Baseline eval run JSON (defaults to previous in evals/)",
    )
    parser.add_argument(
        "--allow-missing-baseline",
        action="store_true",
        help="Pass when baseline/current runs are unavailable",
    )
    parser.add_argument(
        "--max-pass-at-1-drop",
        type=float,
        default=0.03,
        help="Maximum allowed absolute pass@1 drop",
    )
    parser.add_argument(
        "--max-mean-task-duration-increase",
        type=float,
        default=0.40,
        help="Maximum allowed fractional increase in mean task duration",
    )
    parser.add_argument(
        "--max-cost-increase",
        type=float,
        default=0.50,
        help="Maximum allowed fractional increase in total cost",
    )
    parser.add_argument("--report-path", default="", help="Optional report path")
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"eval-trend-gate-{stamp}.json"


def load_json(path: pathlib.Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def duration_to_secs(raw: Any) -> float:
    if raw is None:
        return 0.0
    if isinstance(raw, (int, float)):
        return float(raw)
    if isinstance(raw, str):
        # Rust Duration may serialize as "12.345s"
        text = raw.strip().lower()
        if text.endswith("s"):
            text = text[:-1]
        try:
            return float(text)
        except ValueError:
            return 0.0
    if isinstance(raw, dict):
        secs = raw.get("secs") or raw.get("seconds") or 0
        nanos = raw.get("nanos") or raw.get("nanoseconds") or 0
        try:
            return float(secs) + float(nanos) / 1_000_000_000.0
        except (TypeError, ValueError):
            return 0.0
    return 0.0


def safe_float(raw: Any, default: float = 0.0) -> float:
    try:
        return float(raw)
    except (TypeError, ValueError):
        return default


def extract_metrics(record: dict[str, Any]) -> dict[str, float]:
    metrics = record.get("metrics", {}) if isinstance(record, dict) else {}
    total = max(1.0, safe_float(metrics.get("total", 0.0), 0.0))
    total_duration = duration_to_secs(metrics.get("total_duration"))
    return {
        "total": total,
        "pass_at_1": safe_float(metrics.get("pass_at_1", 0.0), 0.0),
        "mean_task_duration_secs": total_duration / total,
        "total_cost_usd": safe_float(metrics.get("total_cost_usd", 0.0), 0.0),
    }


def find_latest_eval_files(evals_dir: pathlib.Path) -> list[pathlib.Path]:
    if not evals_dir.exists():
        return []
    files = [p for p in evals_dir.glob("*.json") if p.is_file()]
    files.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    return files


def rel_change(current: float, baseline: float) -> float:
    if baseline <= 0.0:
        return 0.0 if current <= 0.0 else 1.0
    return (current - baseline) / baseline


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    report_path = (
        pathlib.Path(args.report_path).expanduser().resolve()
        if args.report_path
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)

    evals_dir = repo_root / "evals"
    latest = find_latest_eval_files(evals_dir)
    current_path = pathlib.Path(args.current).expanduser().resolve() if args.current else None
    baseline_path = pathlib.Path(args.baseline).expanduser().resolve() if args.baseline else None
    if current_path is None and latest:
        current_path = latest[0]
    if baseline_path is None and len(latest) > 1:
        baseline_path = latest[1]

    if current_path is None or baseline_path is None or not current_path.exists() or not baseline_path.exists():
        ok = bool(args.allow_missing_baseline)
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "ok": ok,
            "reason": "missing_eval_inputs",
            "allow_missing_baseline": bool(args.allow_missing_baseline),
            "current_path": str(current_path) if current_path else None,
            "baseline_path": str(baseline_path) if baseline_path else None,
        }
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            status = "PASSED" if ok else "FAILED"
            print(f"[eval-trend-gate] {status} (missing baseline/current report)")
            print(f"[eval-trend-gate] Report: {report_path}")
        return 0 if ok else 1

    current = load_json(current_path)
    baseline = load_json(baseline_path)
    current_metrics = extract_metrics(current)
    baseline_metrics = extract_metrics(baseline)

    pass_drop = baseline_metrics["pass_at_1"] - current_metrics["pass_at_1"]
    duration_increase = rel_change(
        current_metrics["mean_task_duration_secs"],
        baseline_metrics["mean_task_duration_secs"],
    )
    cost_increase = rel_change(current_metrics["total_cost_usd"], baseline_metrics["total_cost_usd"])

    checks = [
        {
            "name": "pass_at_1_drop",
            "value": pass_drop,
            "limit": args.max_pass_at_1_drop,
            "ok": pass_drop <= args.max_pass_at_1_drop,
        },
        {
            "name": "mean_task_duration_increase",
            "value": duration_increase,
            "limit": args.max_mean_task_duration_increase,
            "ok": duration_increase <= args.max_mean_task_duration_increase,
        },
        {
            "name": "total_cost_increase",
            "value": cost_increase,
            "limit": args.max_cost_increase,
            "ok": cost_increase <= args.max_cost_increase,
        },
    ]
    gate_ok = all(check["ok"] for check in checks)
    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "ok": gate_ok,
        "current_path": str(current_path),
        "baseline_path": str(baseline_path),
        "current_metrics": current_metrics,
        "baseline_metrics": baseline_metrics,
        "checks": checks,
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if gate_ok else "FAILED"
        print(
            f"[eval-trend-gate] {status} "
            f"(pass_drop={pass_drop:.4f}, duration_increase={duration_increase:.3f}, cost_increase={cost_increase:.3f})"
        )
        print(f"[eval-trend-gate] Report: {report_path}")

    return 0 if gate_ok else 1


if __name__ == "__main__":
    sys.exit(main())
