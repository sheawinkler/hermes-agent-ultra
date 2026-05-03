#!/usr/bin/env python3
"""Generate a comprehensive parity proof artifact and evaluate thresholds."""

from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path
from typing import Any


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def gate_mode(thresholds: dict[str, Any]) -> str:
    mode = str(thresholds.get("gate_mode", "")).strip().lower()
    return mode if mode else "legacy"


def gate_metric_thresholds(thresholds: dict[str, Any]) -> dict[str, Any]:
    metric_thresholds = thresholds.get("metric_thresholds")
    if isinstance(metric_thresholds, dict):
        return metric_thresholds
    out: dict[str, Any] = {}
    for key, value in thresholds.items():
        if key in {"required_workstreams_complete", "gate_mode", "special_rules"}:
            continue
        out[key] = value
    return out


def evaluate_gate(metrics: dict[str, float], thresholds: dict[str, Any]) -> dict[str, Any]:
    checks: list[dict[str, Any]] = []
    passed = True
    for key, limit in gate_metric_thresholds(thresholds).items():
        actual = metrics.get(key)
        if actual is None:
            checks.append({"metric": key, "status": "missing", "actual": None, "limit": limit})
            passed = False
            continue
        if key.startswith("min_"):
            ok = actual >= float(limit)
            checks.append({"metric": key, "status": "pass" if ok else "fail", "actual": actual, "limit": limit})
            passed = passed and ok
        else:
            ok = actual <= float(limit)
            checks.append({"metric": key, "status": "pass" if ok else "fail", "actual": actual, "limit": limit})
            passed = passed and ok
    return {"pass": passed, "checks": checks}


def _check_by_metric(checks: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for check in checks:
        metric = str(check.get("metric", "")).strip()
        if metric:
            out[metric] = check
    return out


def apply_ci_special_rules(
    metrics: dict[str, float],
    ci_gate: dict[str, Any],
    release_gate: dict[str, Any],
    ci_thresholds: dict[str, Any],
) -> None:
    special_rules = ci_thresholds.get("special_rules")
    if not isinstance(special_rules, dict):
        return

    rule_cfg = special_rules.get("allow_files_only_upstream_overshoot_when_functional_clean")
    if not isinstance(rule_cfg, dict):
        return
    if not bool(rule_cfg.get("enabled", False)):
        return

    checks = ci_gate.get("checks", [])
    if not isinstance(checks, list):
        return
    failed_checks = [c for c in checks if c.get("status") == "fail"]
    failed_metrics = {str(c.get("metric", "")).strip() for c in failed_checks}
    if failed_metrics != {"max_files_only_upstream"}:
        return

    check_map = _check_by_metric(checks)
    files_only_upstream_check = check_map.get("max_files_only_upstream")
    if not isinstance(files_only_upstream_check, dict):
        return

    actual_raw = files_only_upstream_check.get("actual")
    limit_raw = files_only_upstream_check.get("limit")
    try:
        actual = float(actual_raw)
        limit = float(limit_raw)
    except (TypeError, ValueError):
        return

    overshoot = max(0.0, actual - limit)
    max_overshoot = float(rule_cfg.get("max_overshoot", 0.0))
    if overshoot > max_overshoot:
        return

    if bool(rule_cfg.get("requires_release_gate_pass", True)) and not bool(
        release_gate.get("pass", False)
    ):
        return

    unowned_limit = float(rule_cfg.get("requires_max_unowned_divergences", 0))
    review_limit = float(rule_cfg.get("requires_max_divergence_review_overdue", 0))
    pending_limit = float(rule_cfg.get("requires_max_queue_pending_commits", 100))
    ratio_limit = float(rule_cfg.get("requires_min_test_intent_mapping_ratio", 0.9))
    if metrics.get("max_unowned_divergences", 0.0) > unowned_limit:
        return
    if metrics.get("max_divergence_review_overdue", 0.0) > review_limit:
        return
    if metrics.get("max_queue_pending_commits", 0.0) > pending_limit:
        return
    if metrics.get("min_test_intent_mapping_ratio", 0.0) < ratio_limit:
        return

    files_only_upstream_check["status"] = "warn"
    files_only_upstream_check["special_rule"] = (
        "allow_files_only_upstream_overshoot_when_functional_clean"
    )
    files_only_upstream_check["overshoot"] = overshoot
    files_only_upstream_check["max_overshoot"] = max_overshoot
    ci_gate["pass"] = True
    ci_gate.setdefault("special_rules_applied", []).append(
        {
            "rule": "allow_files_only_upstream_overshoot_when_functional_clean",
            "metric": "max_files_only_upstream",
            "actual": actual,
            "limit": limit,
            "overshoot": overshoot,
            "max_overshoot": max_overshoot,
            "reason": "functional parity clean; upstream-only file growth treated as drift warning",
        }
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate global parity proof report.")
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument(
        "--thresholds",
        default="docs/parity/global-parity-thresholds.json",
        type=Path,
        help="Parity thresholds file (relative to repo root)",
    )
    parser.add_argument(
        "--parity-matrix",
        default="docs/parity/parity-matrix.json",
        type=Path,
        help="Parity matrix JSON file",
    )
    parser.add_argument(
        "--workstream-status",
        default="docs/parity/workstream-status.json",
        type=Path,
        help="Workstream status JSON file",
    )
    parser.add_argument(
        "--intent-mapping",
        default="docs/parity/test-intent-mapping.json",
        type=Path,
        help="Test intent mapping JSON file",
    )
    parser.add_argument(
        "--adapter-matrix",
        default="docs/parity/adapter-feature-matrix.json",
        type=Path,
        help="Adapter feature matrix JSON file",
    )
    parser.add_argument(
        "--shared-diff-classification",
        default="docs/parity/shared-different-classification.json",
        type=Path,
        help="Shared-different classification JSON file",
    )
    parser.add_argument(
        "--divergence-validation",
        default="docs/parity/divergence-validation.json",
        type=Path,
        help="Divergence validation report JSON file",
    )
    parser.add_argument(
        "--patch-queue",
        default="docs/parity/upstream-missing-queue.json",
        type=Path,
        help="Upstream patch queue JSON file",
    )
    parser.add_argument(
        "--out-json",
        default="docs/parity/global-parity-proof.json",
        type=Path,
        help="Output proof JSON path",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/global-parity-proof.md",
        type=Path,
        help="Output proof markdown path",
    )
    parser.add_argument("--check-ci", action="store_true", help="Fail if CI gate fails")
    parser.add_argument("--check-release", action="store_true", help="Fail if release gate fails")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    thresholds = load_json((repo_root / args.thresholds).resolve())
    parity = load_json((repo_root / args.parity_matrix).resolve())
    ws = load_json((repo_root / args.workstream_status).resolve())
    intents = load_json((repo_root / args.intent_mapping).resolve())
    adapters = load_json((repo_root / args.adapter_matrix).resolve())
    shared_diff = load_json((repo_root / args.shared_diff_classification).resolve())
    divergence = load_json((repo_root / args.divergence_validation).resolve())
    patch_queue = load_json((repo_root / args.patch_queue).resolve())

    parity_summary = parity.get("summary", {})
    intent_summary = intents.get("summary", {})
    adapter_summary = adapters.get("summary", {})
    divergence_summary = divergence.get("summary", {})
    queue_summary = patch_queue.get("summary", {})
    ws_states = ws.get("states", {})
    shared_diff_items = {str(i.get("path", "")) for i in shared_diff.get("items", [])}
    parity_shared = {str(i.get("path", "")) for i in parity.get("top_shared_different", [])}

    gpar_completion = {
        "GPAR-01": float(intent_summary.get("mapping_ratio", 0.0)) >= 0.9,
        "GPAR-02": any(i.get("id") == "skills-management-contract" and i.get("mapped") for i in intents.get("intents", [])),
        "GPAR-03": any(i.get("id") == "rust-cli-tui-primary-ux-surface" for i in parity.get("intentional_divergence", [])),
        "GPAR-04": int(adapter_summary.get("non_rust_native", 0)) == 0,
        "GPAR-05": any(i.get("id") == "environment-lifecycle-contract" and i.get("mapped") for i in intents.get("intents", []))
        and any(i.get("id") == "tool-call-parser-contract" and i.get("mapped") for i in intents.get("intents", [])),
        "GPAR-06": parity_shared.issubset(shared_diff_items),
        "GPAR-07": int(queue_summary.get("total_commits", 0)) > 0,
        "GPAR-08": int(divergence_summary.get("errors", 0)) == 0
        and int(divergence_summary.get("unowned", 0)) == 0
        and int(divergence_summary.get("review_overdue", 0)) == 0,
        "GPAR-09": True,
    }

    metrics = {
        "max_commits_behind": float(parity_summary.get("commits_behind", 0)),
        "max_upstream_patch_missing": float(parity_summary.get("upstream_patch_missing", 0)),
        "max_files_only_upstream": float(parity_summary.get("files_only_upstream", 0)),
        "max_shared_different": float(parity_summary.get("files_shared_different", 0)),
        "max_unowned_divergences": float(divergence_summary.get("unowned", 0)),
        "max_divergence_review_overdue": float(divergence_summary.get("review_overdue", 0)),
        "min_test_intent_mapping_ratio": float(intent_summary.get("mapping_ratio", 0.0)),
        "max_queue_pending_commits": float(
            queue_summary.get("by_disposition", {}).get("pending", 0)
        ),
    }

    ci_gate = evaluate_gate(metrics, thresholds.get("ci_thresholds", {}))
    release_gate = evaluate_gate(metrics, thresholds.get("release_thresholds", {}))
    ci_gate["mode"] = gate_mode(thresholds.get("ci_thresholds", {}))
    release_gate["mode"] = gate_mode(thresholds.get("release_thresholds", {}))

    required = thresholds.get("release_thresholds", {}).get("required_workstreams_complete", [])
    required_ok = all(bool(gpar_completion.get(ws_id, False)) for ws_id in required)
    if not required_ok:
        release_gate["pass"] = False
        release_gate["checks"].append(
            {
                "metric": "required_workstreams_complete",
                "status": "fail",
                "actual": {k: bool(gpar_completion.get(k, False)) for k in required},
                "limit": "all true",
            }
        )
    else:
        release_gate["checks"].append(
            {
                "metric": "required_workstreams_complete",
                "status": "pass",
                "actual": {k: bool(gpar_completion.get(k, False)) for k in required},
                "limit": "all true",
            }
        )

    apply_ci_special_rules(
        metrics=metrics,
        ci_gate=ci_gate,
        release_gate=release_gate,
        ci_thresholds=thresholds.get("ci_thresholds", {}),
    )

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "program": thresholds.get("program", {}),
        "metrics": metrics,
        "gpar_completion": gpar_completion,
        "ws_legacy_states": ws_states,
        "ci_gate": ci_gate,
        "release_gate": release_gate,
        "sources": {
            "thresholds": str(args.thresholds),
            "parity_matrix": str(args.parity_matrix),
            "workstream_status": str(args.workstream_status),
            "intent_mapping": str(args.intent_mapping),
            "adapter_matrix": str(args.adapter_matrix),
            "shared_diff_classification": str(args.shared_diff_classification),
            "divergence_validation": str(args.divergence_validation),
            "patch_queue": str(args.patch_queue),
        },
        "queue_summary": queue_summary,
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md = [
        "# Global Parity Proof",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "## Gate Status",
        "",
        f"- CI gate: **{'PASS' if ci_gate['pass'] else 'FAIL'}**",
        f"- CI gate mode: `{ci_gate.get('mode', 'legacy')}`",
        f"- Release gate: **{'PASS' if release_gate['pass'] else 'FAIL'}**",
        f"- Release gate mode: `{release_gate.get('mode', 'legacy')}`",
        "",
        "## Metrics",
        "",
        "| Metric | Value |",
        "| --- | ---: |",
    ]
    for key, value in metrics.items():
        md.append(f"| `{key}` | {value} |")
    md.append("")
    md.append("## GPAR Ticket Completion")
    md.append("")
    md.append("| Ticket | Complete |")
    md.append("| --- | --- |")
    for key in sorted(gpar_completion.keys()):
        md.append(f"| `{key}` | {'yes' if gpar_completion[key] else 'no'} |")
    md.append("")

    md.append("## Queue Summary")
    md.append("")
    md.append(
        f"- Upstream missing commits tracked: `{queue_summary.get('total_commits', 0)}`."
    )
    by_ticket = queue_summary.get("by_target_ticket", {})
    if isinstance(by_ticket, dict):
        md.append("- By target ticket:")
        for ticket, count in sorted(by_ticket.items(), key=lambda kv: kv[0]):
            md.append(f"  - `#{ticket}`: `{count}`")
    md.append("")
    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")

    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")

    if args.check_release and not release_gate["pass"]:
        return 1
    if args.check_ci and not ci_gate["pass"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
