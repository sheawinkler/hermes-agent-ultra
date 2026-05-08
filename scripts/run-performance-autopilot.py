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
        "--mcp-cmd",
        default="cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture",
        help="MCP stale-transport recovery regression command",
    )
    parser.add_argument(
        "--contextlattice-cmd",
        default=(
            "python3 /Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py "
            "preflight hermes-agent-ultra runbooks/alpha/objective "
            "\"hermes-ultra contextlattice intelligence preflight\""
        ),
        help="ContextLattice preflight/intelligence telemetry command",
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


def run_shell(command: str, cwd: pathlib.Path, max_tail: int = 6000) -> dict[str, Any]:
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
        "stdout_tail": proc.stdout[-max_tail:],
        "stderr_tail": proc.stderr[-max_tail:],
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


def parse_contextlattice_payload(section: dict[str, Any]) -> dict[str, Any] | None:
    raw = (section.get("stdout_tail") or "").strip()
    if not raw:
        return None
    start = raw.find("{")
    end = raw.rfind("}")
    if start < 0 or end <= start:
        return None
    blob = raw[start : end + 1]
    try:
        parsed = json.loads(blob)
    except Exception:
        return None
    if isinstance(parsed, dict):
        return parsed
    return None


def contextlattice_summary(payload: dict[str, Any] | None) -> dict[str, Any]:
    summary = {
        "healthy": False,
        "warnings": 0,
        "python_fallbacks": 0,
        "route_owner_class": "",
        "source_counts": {},
        "queue_pending_total": 0,
    }
    if not payload:
        return summary
    health = payload.get("health") if isinstance(payload.get("health"), dict) else {}
    summary["healthy"] = bool(health.get("ok"))
    warnings = payload.get("warnings")
    if isinstance(warnings, list):
        summary["warnings"] = len(warnings)
    retrieval = payload.get("context_pack", {}).get("retrieval")
    if isinstance(retrieval, dict):
        summary["route_owner_class"] = str(retrieval.get("route_owner_class") or "")
        source_counts = retrieval.get("source_counts")
        if isinstance(source_counts, dict):
            summary["source_counts"] = source_counts
        fallbacks = retrieval.get("fallback_counts")
        if isinstance(fallbacks, dict):
            summary["python_fallbacks"] = int(fallbacks.get("python_hot_path_total") or 0)
    status = payload.get("status")
    if isinstance(status, dict):
        queue = status.get("queue")
        if isinstance(queue, dict):
            summary["queue_pending_total"] = int(queue.get("pendingTotal") or 0)
    return summary


def build_recommendations(
    hotpath: dict[str, Any],
    eval_gate: dict[str, Any],
    mcp_gate: dict[str, Any],
    context_gate: dict[str, Any],
) -> list[dict[str, str]]:
    recs: list[dict[str, str]] = []
    ns = parse_hotpath_ns((hotpath.get("stdout_tail") or "") + "\n" + (hotpath.get("stderr_tail") or ""))
    ctx_payload = parse_contextlattice_payload(context_gate)
    ctx_summary = contextlattice_summary(ctx_payload)

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

    if not mcp_gate.get("ok"):
        recs.append(
            {
                "id": "MCP_STALE_RECOVERY_FAIL",
                "severity": "P1",
                "title": "MCP stale transport recovery regression",
                "recommendation": "Run `cargo test -p hermes-mcp` and restore reconnect-on-stale behavior before promotion.",
            }
        )

    if not context_gate.get("ok"):
        recs.append(
            {
                "id": "CONTEXTLATTICE_PREFLIGHT_FAIL",
                "severity": "P0",
                "title": "ContextLattice preflight failed",
                "recommendation": "Run `python3 /Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py preflight hermes-agent-ultra runbooks/alpha/objective` and resolve retrieval/service health before promotion.",
            }
        )
    elif not ctx_summary["healthy"]:
        recs.append(
            {
                "id": "CONTEXTLATTICE_UNHEALTHY",
                "severity": "P1",
                "title": "ContextLattice health is degraded",
                "recommendation": "Use `/objective context max` and confirm orchestrator health/retrieval lanes before long-running objective loops.",
            }
        )
    if ctx_summary["python_fallbacks"] > 0:
        recs.append(
            {
                "id": "CONTEXTLATTICE_PYTHON_FALLBACK",
                "severity": "P1",
                "title": "ContextLattice retrieval fallback detected",
                "recommendation": "Investigate non-native fallback causes and keep Go/Rust lanes hot to avoid degraded memory-intelligence behavior.",
            }
        )
    if (
        isinstance(ctx_summary["source_counts"], dict)
        and not ctx_summary["source_counts"]
        and int(ctx_summary["python_fallbacks"]) == 0
    ):
        recs.append(
            {
                "id": "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
                "severity": "P1",
                "title": "ContextLattice source coverage is empty",
                "recommendation": "Use broader same-project context-pack and ensure topic rollups/primary stores return at least one grounded hit.",
            }
        )
    if int(ctx_summary["queue_pending_total"]) > 8:
        recs.append(
            {
                "id": "CONTEXTLATTICE_QUEUE_PRESSURE",
                "severity": "P2",
                "title": "ContextLattice queue pressure elevated",
                "recommendation": "Reduce write burst size or raise checkpoint spacing for long loops until pending queue normalizes.",
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
        f"- intelligence_index: `{report.get('intelligence_index', 0):.2f}`",
        f"- performance_index: `{report.get('performance_index', 0):.2f}`",
        f"- adaptive_index: `{report.get('adaptive_index', 0):.2f}`",
        f"- profile_recommendation: `{report.get('profile_recommendation', 'balanced')}`",
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
    actions = report.get("adaptive_actions") or []
    if actions:
        lines.extend(["", "## Adaptive Actions"])
        for action in actions:
            lines.append(f"- `{action['key']}={action['value']}` ({action['reason']})")
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
    if any(
        key in rec_ids
        for key in (
            "CONTEXTLATTICE_PREFLIGHT_FAIL",
            "CONTEXTLATTICE_UNHEALTHY",
            "CONTEXTLATTICE_PYTHON_FALLBACK",
            "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
        )
    ):
        lines.extend(
            [
                "HERMES_CONTEXTLATTICE_MODE=max",
                "HERMES_CONTEXTLATTICE_RETRIEVAL_MODE=deep",
                "HERMES_CONTEXTLATTICE_REQUIRE_READBACK=1",
            ]
        )
    if rec_ids == {"PERF_STABLE"}:
        lines.append("HERMES_PERF_AUTOPILOT_STATUS=stable")
    lines.append(f"HERMES_PERF_AUTOPILOT_PROFILE={report.get('profile_recommendation', 'balanced')}")
    for action in report.get("adaptive_actions", []):
        key = action.get("key")
        value = action.get("value")
        if key and value is not None:
            lines.append(f"{key}={value}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def compute_adaptive_indexes(
    hotpath: dict[str, Any],
    eval_gate: dict[str, Any],
    mcp_gate: dict[str, Any],
    context_gate: dict[str, Any],
    recommendations: list[dict[str, str]],
) -> dict[str, Any]:
    ns = parse_hotpath_ns((hotpath.get("stdout_tail") or "") + "\n" + (hotpath.get("stderr_tail") or ""))
    checks = [hotpath.get("ok"), eval_gate.get("ok"), mcp_gate.get("ok"), context_gate.get("ok")]
    pass_count = sum(1 for ok in checks if ok)
    fail_count = len(checks) - pass_count
    base_performance = 100.0
    if ns is not None and ns > 12000:
        # 12k ns target, penalize by overflow ratio capped to 30 points.
        overflow_ratio = min((ns - 12000) / 12000, 3.0)
        base_performance -= min(30.0, overflow_ratio * 10.0)
    if not hotpath.get("ok"):
        base_performance -= 35.0
    if not mcp_gate.get("ok"):
        base_performance -= 20.0
    if not context_gate.get("ok"):
        base_performance -= 25.0
    base_performance = max(0.0, min(100.0, base_performance))

    severity_weight = {"P0": 22.0, "P1": 12.0, "P2": 6.0, "P3": 2.0}
    penalty = 0.0
    for rec in recommendations:
        penalty += severity_weight.get(str(rec.get("severity", "P3")).upper(), 4.0)
    # Intelligence index rewards clean eval/recovery behavior.
    base_intelligence = 100.0 - penalty
    if not eval_gate.get("ok"):
        base_intelligence -= 18.0
    if not context_gate.get("ok"):
        base_intelligence -= 20.0
    ctx_payload = parse_contextlattice_payload(context_gate)
    ctx_summary = contextlattice_summary(ctx_payload)
    if ctx_summary["python_fallbacks"] > 0:
        base_intelligence -= min(12.0, float(ctx_summary["python_fallbacks"]))
    if (
        isinstance(ctx_summary["source_counts"], dict)
        and not ctx_summary["source_counts"]
        and int(ctx_summary["python_fallbacks"]) == 0
    ):
        base_intelligence -= 10.0
    if int(ctx_summary["queue_pending_total"]) > 8:
        base_intelligence -= 8.0
    base_intelligence = max(0.0, min(100.0, base_intelligence))

    adaptive_index = round(base_performance * 0.55 + base_intelligence * 0.45, 2)
    performance_index = round(base_performance, 2)
    intelligence_index = round(base_intelligence, 2)

    if fail_count >= 2:
        profile = "safety"
    elif not eval_gate.get("ok"):
        profile = "quality"
    elif not context_gate.get("ok"):
        profile = "quality"
    elif not mcp_gate.get("ok"):
        profile = "reliability"
    elif ns is not None and ns > 12000:
        profile = "throughput"
    else:
        profile = "balanced"

    adaptive_actions: list[dict[str, str]] = []
    adaptive_actions.append(
        {
            "key": "HERMES_PERF_AUTOPILOT_PROFILE",
            "value": profile,
            "reason": "profile recommendation from adaptive index",
        }
    )
    if profile == "throughput":
        adaptive_actions.extend(
            [
                {"key": "HERMES_TOOL_POLICY_PRESET", "value": "standard", "reason": "reduce policy hot-path overhead"},
                {"key": "HERMES_MODEL_CATALOG_GUARD", "value": "1", "reason": "avoid invalid model retries"},
            ]
        )
    elif profile == "quality":
        adaptive_actions.extend(
            [
                {"key": "HERMES_REPLAY_ENABLED", "value": "1", "reason": "capture deterministic replay for eval failures"},
                {"key": "HERMES_MODEL_AUTO_REMEDIATE", "value": "1", "reason": "promote self-heal recommendation loop"},
            ]
        )
    elif profile == "reliability":
        adaptive_actions.append(
            {"key": "HERMES_TOOL_POLICY_MODE", "value": "enforce", "reason": "stabilize stale transport/recovery behavior"}
        )
    elif profile == "safety":
        adaptive_actions.extend(
            [
                {"key": "HERMES_TOOL_POLICY_MODE", "value": "enforce", "reason": "strict policy posture under multi-check failure"},
                {"key": "HERMES_REPLAY_ENABLED", "value": "1", "reason": "preserve incident evidence during degraded state"},
            ]
        )
    else:
        adaptive_actions.append(
            {"key": "HERMES_PERF_AUTOPILOT_STATUS", "value": "stable", "reason": "all checks stable"}
        )

    return {
        "performance_index": performance_index,
        "intelligence_index": intelligence_index,
        "adaptive_index": adaptive_index,
        "profile_recommendation": profile,
        "adaptive_actions": adaptive_actions,
    }


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
    mcp_gate = run_shell(args.mcp_cmd, repo_root)
    context_gate = run_shell(args.contextlattice_cmd, repo_root, max_tail=240000)
    recommendations = build_recommendations(hotpath, eval_gate, mcp_gate, context_gate)
    ok = all(section.get("ok") for section in [hotpath, eval_gate, mcp_gate, context_gate])
    adaptive = compute_adaptive_indexes(
        hotpath,
        eval_gate,
        mcp_gate,
        context_gate,
        recommendations,
    )

    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "ok": ok,
        "sections": {
            "hotpath": hotpath,
            "eval_trend": eval_gate,
            "mcp_stale_recovery": mcp_gate,
            "contextlattice_preflight": context_gate,
        },
        "recommendations": recommendations,
        "performance_index": adaptive["performance_index"],
        "intelligence_index": adaptive["intelligence_index"],
        "adaptive_index": adaptive["adaptive_index"],
        "profile_recommendation": adaptive["profile_recommendation"],
        "adaptive_actions": adaptive["adaptive_actions"],
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
