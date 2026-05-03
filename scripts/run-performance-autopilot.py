#!/usr/bin/env python3
"""Run performance autopilot checks and emit actionable tuning recommendations."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import subprocess
import sys
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--hotpath-cmd",
        default="python3 scripts/run-zero-copy-hotpath-bench.py",
        help="Hot-path benchmark command",
    )
    parser.add_argument(
        "--eval-cmd",
        default="python3 scripts/run-eval-trend-gate.py --allow-missing-baseline",
        help="Eval trend command",
    )
    parser.add_argument(
        "--output-json",
        default="",
        help="Optional JSON report path",
    )
    parser.add_argument(
        "--output-md",
        default="",
        help="Optional markdown report path",
    )
    parser.add_argument(
        "--apply-env",
        default="",
        help="Optional env file path to write recommended knobs",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Return non-zero when checks fail (default: advisory mode, always exits 0)",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def run_shell(command: str, cwd: pathlib.Path) -> dict[str, Any]:
    started = dt.datetime.now(dt.timezone.utc)
    proc = subprocess.run(
        ["bash", "-lc", command],
        cwd=str(cwd),
        capture_output=True,
        text=True,
        check=False,
    )
    finished = dt.datetime.now(dt.timezone.utc)
    return {
        "command": command,
        "exit_code": proc.returncode,
        "ok": proc.returncode == 0,
        "started_at": started.isoformat(),
        "finished_at": finished.isoformat(),
        "duration_ms": int((finished - started).total_seconds() * 1000),
        "stdout_tail": proc.stdout[-6000:],
        "stderr_tail": proc.stderr[-6000:],
    }


def parse_hotpath_ns(stdout: str) -> int | None:
    needle = "tool_policy_hot_path_ns_per_eval="
    idx = stdout.rfind(needle)
    if idx < 0:
        return None
    value = stdout[idx + len(needle):].splitlines()[0].strip()
    if not value.isdigit():
        return None
    return int(value)


def build_recommendations(hotpath: dict[str, Any], eval_gate: dict[str, Any]) -> list[dict[str, str]]:
    recs: list[dict[str, str]] = []
    ns = parse_hotpath_ns((hotpath.get("stdout_tail") or "") + "\n" + (hotpath.get("stderr_tail") or ""))

    if not hotpath.get("ok"):
        recs.append(
            {
                "id": "HOTPATH_FAIL",
                "severity": "P0",
                "title": "Hot-path benchmark failed",
                "recommendation": "Run `cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture` and resolve regressions before release.",
            }
        )
    elif ns is not None and ns > 12000:
        recs.append(
            {
                "id": "HOTPATH_SLOW",
                "severity": "P1",
                "title": "Tool policy hot-path latency above target",
                "recommendation": "Keep `HERMES_TOOL_POLICY_PRESET=standard`, review deny-pattern complexity, and rerun zero-copy bench.",
            }
        )

    if not eval_gate.get("ok"):
        recs.append(
            {
                "id": "EVAL_TREND_FAIL",
                "severity": "P0",
                "title": "Eval trend gate failed",
                "recommendation": "Run `python3 scripts/run-self-evolution-loop.py --json` and address top recommendation before promotion.",
            }
        )

    if not recs:
        recs.append(
            {
                "id": "PERF_STABLE",
                "severity": "P3",
                "title": "Performance checks stable",
                "recommendation": "No immediate tuning required. Keep nightly elite gate cadence.",
            }
        )

    return recs


def default_paths(repo_root: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    out_dir = repo_root / ".sync-reports"
    return (
        out_dir / f"performance-autopilot-{stamp}.json",
        out_dir / f"performance-autopilot-{stamp}.md",
    )


def write_markdown(path: pathlib.Path, report: dict[str, Any]) -> None:
    lines = [
        "# Performance Autopilot Report",
        "",
        f"- generated_at: `{report['generated_at']}`",
        f"- ok: `{report['ok']}`",
        "",
        "## Sections",
    ]
    for name, section in report["sections"].items():
        lines.append(
            f"- `{name}`: {'PASS' if section.get('ok') else 'FAIL'} (exit={section.get('exit_code')})"
        )
    lines.extend(["", "## Recommendations"])
    for rec in report["recommendations"]:
        lines.append(
            f"- **{rec['id']} ({rec['severity']})**: {rec['title']} — {rec['recommendation']}"
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines).strip() + "\n", encoding="utf-8")


def write_env(path: pathlib.Path, report: dict[str, Any]) -> None:
    rec_ids = {rec["id"] for rec in report["recommendations"]}
    lines = [f"# generated_at={report['generated_at']}"]
    if "HOTPATH_SLOW" in rec_ids:
        lines.extend(
            [
                "HERMES_TOOL_POLICY_PRESET=standard",
                "HERMES_TOOL_POLICY_MODE=enforce",
                "HERMES_MODEL_CATALOG_GUARD=1",
            ]
        )
    if "EVAL_TREND_FAIL" in rec_ids:
        lines.extend(
            [
                "HERMES_MODEL_AUTO_REMEDIATE=1",
                "HERMES_REPLAY_ENABLED=1",
            ]
        )
    if rec_ids == {"PERF_STABLE"}:
        lines.append("HERMES_PERF_AUTOPILOT_STATUS=stable")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    default_json, default_md = default_paths(repo_root)
    output_json = pathlib.Path(args.output_json).expanduser().resolve() if args.output_json else default_json
    output_md = pathlib.Path(args.output_md).expanduser().resolve() if args.output_md else default_md

    hotpath = run_shell(args.hotpath_cmd, repo_root)
    eval_gate = run_shell(args.eval_cmd, repo_root)
    recommendations = build_recommendations(hotpath, eval_gate)
    ok = all(section.get("ok") for section in [hotpath, eval_gate])

    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "ok": ok,
        "sections": {
            "hotpath": hotpath,
            "eval_trend": eval_gate,
        },
        "recommendations": recommendations,
        "report_json": str(output_json),
        "report_markdown": str(output_md),
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    write_markdown(output_md, report)

    if args.apply_env:
        env_path = pathlib.Path(args.apply_env).expanduser().resolve()
        write_env(env_path, report)
        report["applied_env"] = str(env_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if ok else "FAILED"
        print(f"[performance-autopilot] {status}")
        print(f"[performance-autopilot] JSON: {output_json}")
        print(f"[performance-autopilot] Markdown: {output_md}")
        if args.apply_env:
            print(f"[performance-autopilot] Env recommendations: {report['applied_env']}")

    if args.strict and not ok:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
