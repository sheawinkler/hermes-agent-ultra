#!/usr/bin/env python3
"""Compare adapter chaos harness reports and fail on regressions."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import sys
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="Baseline chaos report JSON path")
    parser.add_argument("--current", required=True, help="Current chaos report JSON path")
    parser.add_argument("--output", default="", help="Optional output report path")
    parser.add_argument("--json", action="store_true", help="Print JSON report to stdout")
    return parser.parse_args()


def load_report(path: pathlib.Path) -> dict[str, Any]:
    try:
        raw = path.read_text()
    except Exception as exc:
        raise SystemExit(f"failed to read report {path}: {exc}") from exc
    try:
        data = json.loads(raw)
    except Exception as exc:
        raise SystemExit(f"failed to parse report {path}: {exc}") from exc
    if not isinstance(data, dict):
        raise SystemExit(f"report {path} must be a JSON object")
    return data


def outcome_rank(value: str) -> int:
    return {"success": 0, "error": 1}.get(str(value or "").lower(), 2)


def index_runs(report: dict[str, Any]) -> dict[str, dict[str, Any]]:
    runs = report.get("scenario_runs")
    if not isinstance(runs, list):
        return {}
    indexed: dict[str, dict[str, Any]] = {}
    for entry in runs:
        if not isinstance(entry, dict):
            continue
        scenario = str(entry.get("scenario") or "").strip()
        if not scenario:
            continue
        indexed[scenario] = entry
    return indexed


def main() -> int:
    args = parse_args()
    baseline_path = pathlib.Path(args.baseline).expanduser().resolve()
    current_path = pathlib.Path(args.current).expanduser().resolve()
    baseline = load_report(baseline_path)
    current = load_report(current_path)

    regressions: list[dict[str, Any]] = []
    baseline_runs = index_runs(baseline)
    current_runs = index_runs(current)

    if baseline_runs and not current_runs:
        regressions.append(
            {
                "type": "missing_current_scenario_metrics",
                "reason": "baseline has scenario_runs but current does not",
            }
        )

    for scenario, base in baseline_runs.items():
        cur = current_runs.get(scenario)
        if cur is None:
            regressions.append(
                {
                    "type": "scenario_missing",
                    "scenario": scenario,
                    "reason": "scenario present in baseline but missing in current",
                }
            )
            continue
        base_actual = base.get("actual", {}) if isinstance(base.get("actual"), dict) else {}
        cur_actual = cur.get("actual", {}) if isinstance(cur.get("actual"), dict) else {}

        base_attempts = int(base_actual.get("attempts", 0))
        cur_attempts = int(cur_actual.get("attempts", 0))
        if cur_attempts > base_attempts:
            regressions.append(
                {
                    "type": "attempts_regression",
                    "scenario": scenario,
                    "baseline": base_attempts,
                    "current": cur_attempts,
                }
            )

        base_fallback = int(base_actual.get("fallback_calls", 0))
        cur_fallback = int(cur_actual.get("fallback_calls", 0))
        if cur_fallback > base_fallback:
            regressions.append(
                {
                    "type": "fallback_regression",
                    "scenario": scenario,
                    "baseline": base_fallback,
                    "current": cur_fallback,
                }
            )

        base_outcome = str(base_actual.get("outcome", ""))
        cur_outcome = str(cur_actual.get("outcome", ""))
        if outcome_rank(cur_outcome) > outcome_rank(base_outcome):
            regressions.append(
                {
                    "type": "outcome_regression",
                    "scenario": scenario,
                    "baseline": base_outcome,
                    "current": cur_outcome,
                }
            )

        base_error = str(base_actual.get("error") or "").strip()
        cur_error = str(cur_actual.get("error") or "").strip()
        if not base_error and cur_error:
            regressions.append(
                {
                    "type": "new_error",
                    "scenario": scenario,
                    "current_error": cur_error[:400],
                }
            )

    if bool(baseline.get("passed")) and not bool(current.get("passed")):
        regressions.append(
            {
                "type": "overall_pass_regression",
                "baseline_passed": bool(baseline.get("passed")),
                "current_passed": bool(current.get("passed")),
            }
        )

    report = {
        "generated_at": dt.datetime.now(dt.UTC).isoformat(),
        "baseline": str(baseline_path),
        "current": str(current_path),
        "ok": len(regressions) == 0,
        "regressions": regressions,
    }

    output_path: pathlib.Path | None = None
    if args.output:
        output_path = pathlib.Path(args.output).expanduser().resolve()
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        report["output"] = str(output_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if report["ok"] else "FAILED"
        print(
            f"Adapter chaos regression compare {status} "
            f"(regressions={len(regressions)})"
        )
        if output_path is not None:
            print(f"Report: {output_path}")
        if regressions:
            print(json.dumps(regressions, indent=2))

    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
