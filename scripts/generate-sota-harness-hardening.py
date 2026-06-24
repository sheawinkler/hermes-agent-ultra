#!/usr/bin/env python3
"""Generate SOTA harness hardening artifacts for parity release review."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
from pathlib import Path
from typing import Any


TREND_JSON = "docs/parity/harness-trend-ledger.json"
TREND_MD = "docs/parity/harness-trend-ledger.md"
REPLAY_JSON = "docs/parity/contextlattice-replay-evidence-index.json"
REPLAY_MD = "docs/parity/contextlattice-replay-evidence-index.md"
BUDGET_JSON = "docs/parity/harness-budget.json"
BUDGET_MD = "docs/parity/harness-budget.md"


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def run_git(repo_root: Path, args: list[str]) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        return ""
    return proc.stdout.strip()


def metric(payload: dict[str, Any], *path: str, default: Any = 0) -> Any:
    cur: Any = payload
    for key in path:
        if not isinstance(cur, dict):
            return default
        cur = cur.get(key)
    return default if cur is None else cur


def as_int(value: Any) -> int:
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def as_float(value: Any) -> float:
    try:
        return float(value or 0.0)
    except (TypeError, ValueError):
        return 0.0


def source_artifacts() -> dict[str, str]:
    return {
        "global_parity_proof": "docs/parity/global-parity-proof.json",
        "harness_budget": BUDGET_JSON,
        "harness_trend_ledger": TREND_JSON,
        "sota_harness_matrix": "docs/parity/sota-harness-matrix.json",
        "test_coverage_audit": "docs/parity/test-coverage-audit.json",
        "upstream_missing_queue": "docs/parity/upstream-missing-queue.json",
    }


def build_snapshot(
    repo_root: Path,
    coverage: dict[str, Any],
    proof: dict[str, Any],
    queue: dict[str, Any],
    sota: dict[str, Any],
) -> dict[str, Any]:
    queue_by_disp = metric(queue, "summary", "by_disposition", default={})
    local_head = run_git(repo_root, ["rev-parse", "HEAD"])
    if run_git(repo_root, ["status", "--short"]):
        local_head = f"{local_head}+worktree"
    upstream_head = run_git(repo_root, ["rev-parse", "upstream/main"])
    return {
        "recorded_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "local_head": local_head,
        "upstream_head": upstream_head,
        "release_gate_pass": bool(metric(proof, "release_gate", "pass", default=False)),
        "ci_gate_pass": bool(metric(proof, "ci_gate", "pass", default=False)),
        "queue_pending": as_int(queue_by_disp.get("pending", 0)),
        "queue_total": as_int(metric(queue, "summary", "total_commits", default=0)),
        "tracked_behavior_coverage_ratio": as_float(
            metric(coverage, "summary", "tracked_behavior_coverage_ratio", default=0.0)
        ),
        "tracked_behavior_rows": as_int(
            metric(coverage, "summary", "tracked_behavior_rows", default=0)
        ),
        "covered_behavior_rows": as_int(
            metric(coverage, "summary", "covered_behavior_rows", default=0)
        ),
        "rust_test_functions": as_int(
            metric(coverage, "summary", "rust_test_functions", default=0)
        ),
        "coverage_manifest_entries": as_int(
            metric(coverage, "summary", "coverage_manifest_entries", default=0)
        ),
        "missing_rust_test_refs": as_int(
            metric(coverage, "summary", "missing_rust_test_refs", default=0)
        ),
        "sota_domain_coverage_ratio": as_float(
            metric(sota, "summary", "domain_coverage_ratio", default=0.0)
        ),
        "sota_direct_rust_tests": as_int(
            metric(sota, "summary", "direct_rust_tests", default=0)
        ),
        "sota_workflow_replay_steps": as_int(
            metric(sota, "summary", "workflow_replay_steps", default=0)
        ),
        "sota_protocol_cases": as_int(metric(sota, "summary", "protocol_cases", default=0)),
        "sota_fault_scenarios": as_int(metric(sota, "summary", "fault_scenarios", default=0)),
        "sota_critical_gaps": as_int(metric(sota, "gate", "critical_gaps", default=0)),
        "sota_missing_rust_test_refs": as_int(
            metric(sota, "gate", "missing_rust_test_refs", default=0)
        ),
    }


def build_trend_ledger(repo_root: Path, snapshot: dict[str, Any]) -> dict[str, Any]:
    existing = load_json(repo_root / TREND_JSON)
    entries = existing.get("entries", []) if isinstance(existing.get("entries"), list) else []
    current_head = str(snapshot.get("local_head", ""))
    retained = [
        entry
        for entry in entries
        if isinstance(entry, dict) and str(entry.get("local_head", "")) != current_head
    ]
    retained.append(snapshot)
    retained = retained[-64:]
    previous = retained[-2] if len(retained) >= 2 else None
    deltas: dict[str, Any] = {}
    if isinstance(previous, dict):
        for key in [
            "queue_pending",
            "tracked_behavior_rows",
            "covered_behavior_rows",
            "rust_test_functions",
            "coverage_manifest_entries",
            "sota_direct_rust_tests",
            "sota_workflow_replay_steps",
            "sota_protocol_cases",
            "sota_fault_scenarios",
        ]:
            deltas[key] = as_float(snapshot.get(key)) - as_float(previous.get(key))
    return {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "schema_version": 1,
        "purpose": "Track parity coverage and SOTA harness movement over time.",
        "source_artifacts": source_artifacts(),
        "latest": snapshot,
        "delta_from_previous_entry": deltas,
        "entries": retained,
    }


def build_replay_index(
    coverage: dict[str, Any],
    proof: dict[str, Any],
    queue: dict[str, Any],
    sota: dict[str, Any],
) -> dict[str, Any]:
    groups: list[dict[str, Any]] = []
    for domain in sota.get("domains", []):
        if not isinstance(domain, dict):
            continue
        domain_id = str(domain.get("id", "unknown"))
        groups.append(
            {
                "id": domain_id,
                "title": str(domain.get("title", domain_id)),
                "status": str(domain.get("status", "unknown")),
                "why": str(domain.get("why", "")),
                "fixtures": domain.get("fixtures", []),
                "rust_tests": domain.get("rust_tests", []),
                "required_capabilities": domain.get("required_capabilities", []),
                "contextlattice_topic": f"hermes-agent-ultra/parity/sota/{domain_id}",
                "retrieval_query": (
                    f"Hermes Agent Ultra SOTA harness evidence for {domain_id} "
                    "including fixtures, Rust tests, failures, and queue status"
                ),
            }
        )
    return {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "schema_version": 1,
        "purpose": (
            "Index replay, protocol, and fault-injection evidence so ContextLattice "
            "checkpoints can point to exact local artifacts."
        ),
        "source_artifacts": source_artifacts(),
        "latest_gate_state": {
            "release_gate_pass": bool(metric(proof, "release_gate", "pass", default=False)),
            "ci_gate_pass": bool(metric(proof, "ci_gate", "pass", default=False)),
            "test_coverage_gate_pass": bool(metric(coverage, "audit_gate", "pass", default=False)),
            "sota_harness_gate_pass": bool(metric(sota, "gate", "pass", default=False)),
            "queue_pending": as_int(
                metric(queue, "summary", "by_disposition", "pending", default=0)
            ),
        },
        "replay_evidence_groups": groups,
        "contextlattice_checkpoint_template": {
            "project": "hermes-agent-ultra",
            "topic": "parity/sota",
            "command": (
                "contextlattice_write -p hermes-agent-ultra -t parity/sota "
                "-f docs/parity/contextlattice-replay-evidence-index.md --stdin"
            ),
        },
    }


def build_budget(snapshot: dict[str, Any], thresholds: dict[str, Any]) -> dict[str, Any]:
    release_thresholds = thresholds.get("release_thresholds", {})
    release_values = (
        release_thresholds.get("functional_parity", {})
        if isinstance(release_thresholds, dict)
        else {}
    )
    budgets = [
        {
            "id": "queue_pending",
            "actual": snapshot["queue_pending"],
            "limit": as_int(release_values.get("max_queue_pending_commits", 0)),
            "operator": "<=",
            "reason": "release queue must stay fully triaged",
        },
        {
            "id": "tracked_behavior_coverage_ratio",
            "actual": snapshot["tracked_behavior_coverage_ratio"],
            "limit": as_float(
                release_values.get("min_test_coverage_tracked_behavior_ratio", 1.0)
            ),
            "operator": ">=",
            "reason": "all tracked behavior rows must remain covered",
        },
        {
            "id": "sota_domain_coverage_ratio",
            "actual": snapshot["sota_domain_coverage_ratio"],
            "limit": as_float(release_values.get("min_sota_harness_domain_coverage_ratio", 1.0)),
            "operator": ">=",
            "reason": "all SOTA harness domains must remain represented",
        },
        {
            "id": "rust_test_functions",
            "actual": snapshot["rust_test_functions"],
            "limit": 4500,
            "operator": "<=",
            "reason": "cross-version review required before the Rust test surface grows another large tranche",
        },
        {
            "id": "coverage_manifest_entries",
            "actual": snapshot["coverage_manifest_entries"],
            "limit": 600,
            "operator": "<=",
            "reason": "coverage manifest growth must stay reviewable across releases",
        },
        {
            "id": "sota_workflow_replay_steps",
            "actual": snapshot["sota_workflow_replay_steps"],
            "limit": 12,
            "operator": "<=",
            "reason": "workflow replay growth needs explicit review before it becomes slow",
        },
        {
            "id": "sota_protocol_cases",
            "actual": snapshot["sota_protocol_cases"],
            "limit": 20,
            "operator": "<=",
            "reason": "protocol fixture cases should remain bounded and auditable",
        },
        {
            "id": "sota_fault_scenarios",
            "actual": snapshot["sota_fault_scenarios"],
            "limit": 16,
            "operator": "<=",
            "reason": "fault-injection expansion should stay deterministic",
        },
    ]
    for item in budgets:
        if item["operator"] == "<=":
            item["pass"] = as_float(item["actual"]) <= as_float(item["limit"])
        else:
            item["pass"] = as_float(item["actual"]) >= as_float(item["limit"])
    return {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "schema_version": 1,
        "purpose": "Cross-version review budget for SOTA harness and coverage growth.",
        "source_artifacts": source_artifacts(),
        "snapshot": snapshot,
        "budgets": budgets,
        "gate": {
            "pass": all(bool(item["pass"]) for item in budgets),
            "failures": [item["id"] for item in budgets if not item["pass"]],
        },
    }


def render_trend_md(payload: dict[str, Any]) -> str:
    latest = payload["latest"]
    lines = [
        "# Harness Trend Ledger",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "## Latest",
        "",
        "| Metric | Value |",
        "| --- | ---: |",
    ]
    for key in [
        "queue_pending",
        "queue_total",
        "tracked_behavior_coverage_ratio",
        "tracked_behavior_rows",
        "covered_behavior_rows",
        "rust_test_functions",
        "coverage_manifest_entries",
        "sota_domain_coverage_ratio",
        "sota_direct_rust_tests",
        "sota_workflow_replay_steps",
        "sota_protocol_cases",
        "sota_fault_scenarios",
    ]:
        lines.append(f"| `{key}` | {latest.get(key, 0)} |")
    lines.extend(["", "## Entries", "", "| Recorded | Local head | Queue pending | Coverage | SOTA domains |"])
    lines.append("| --- | --- | ---: | ---: | ---: |")
    for entry in payload["entries"]:
        head = str(entry.get("local_head", ""))[:12]
        lines.append(
            f"| `{entry.get('recorded_at_utc', '')}` | `{head}` | "
            f"{entry.get('queue_pending', 0)} | "
            f"{entry.get('tracked_behavior_coverage_ratio', 0)} | "
            f"{entry.get('sota_domain_coverage_ratio', 0)} |"
        )
    lines.append("")
    return "\n".join(lines)


def render_replay_md(payload: dict[str, Any]) -> str:
    gate = payload["latest_gate_state"]
    lines = [
        "# ContextLattice Replay Evidence Index",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "## Gate State",
        "",
        f"- Release gate: **{'PASS' if gate['release_gate_pass'] else 'FAIL'}**",
        f"- CI gate: **{'PASS' if gate['ci_gate_pass'] else 'FAIL'}**",
        f"- Test coverage gate: **{'PASS' if gate['test_coverage_gate_pass'] else 'FAIL'}**",
        f"- SOTA harness gate: **{'PASS' if gate['sota_harness_gate_pass'] else 'FAIL'}**",
        f"- Queue pending: `{gate['queue_pending']}`",
        "",
        "## Replay Evidence Groups",
        "",
        "| Domain | Status | Fixtures | Rust tests | ContextLattice topic |",
        "| --- | --- | ---: | ---: | --- |",
    ]
    for group in payload["replay_evidence_groups"]:
        lines.append(
            f"| `{group['id']}` | `{group['status']}` | "
            f"{len(group.get('fixtures', []))} | {len(group.get('rust_tests', []))} | "
            f"`{group['contextlattice_topic']}` |"
        )
    lines.extend(
        [
            "",
            "## Checkpoint Template",
            "",
            f"`{payload['contextlattice_checkpoint_template']['command']}`",
            "",
        ]
    )
    return "\n".join(lines)


def render_budget_md(payload: dict[str, Any]) -> str:
    gate = payload["gate"]
    lines = [
        "# Harness Budget",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        f"Budget gate: **{'PASS' if gate['pass'] else 'FAIL'}**",
        "",
        "| Budget | Actual | Operator | Limit | Status |",
        "| --- | ---: | --- | ---: | --- |",
    ]
    for item in payload["budgets"]:
        lines.append(
            f"| `{item['id']}` | {item['actual']} | `{item['operator']}` | "
            f"{item['limit']} | {'PASS' if item['pass'] else 'FAIL'} |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument("--check", action="store_true", help="Fail if generated gates fail")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    coverage = load_json(repo_root / "docs/parity/test-coverage-audit.json")
    proof = load_json(repo_root / "docs/parity/global-parity-proof.json")
    queue = load_json(repo_root / "docs/parity/upstream-missing-queue.json")
    sota = load_json(repo_root / "docs/parity/sota-harness-matrix.json")
    thresholds = load_json(repo_root / "docs/parity/global-parity-thresholds.json")

    snapshot = build_snapshot(repo_root, coverage, proof, queue, sota)
    trend = build_trend_ledger(repo_root, snapshot)
    replay = build_replay_index(coverage, proof, queue, sota)
    budget = build_budget(snapshot, thresholds)

    if args.check:
        print(f"Checked {repo_root / TREND_JSON}")
        print(f"Checked {repo_root / TREND_MD}")
        print(f"Checked {repo_root / REPLAY_JSON}")
        print(f"Checked {repo_root / REPLAY_MD}")
        print(f"Checked {repo_root / BUDGET_JSON}")
        print(f"Checked {repo_root / BUDGET_MD}")
        if not budget["gate"]["pass"]:
            return 1
        return 0

    write_json(repo_root / TREND_JSON, trend)
    (repo_root / TREND_MD).write_text(render_trend_md(trend), encoding="utf-8")
    write_json(repo_root / REPLAY_JSON, replay)
    (repo_root / REPLAY_MD).write_text(render_replay_md(replay), encoding="utf-8")
    write_json(repo_root / BUDGET_JSON, budget)
    (repo_root / BUDGET_MD).write_text(render_budget_md(budget), encoding="utf-8")

    print(f"Wrote {repo_root / TREND_JSON}")
    print(f"Wrote {repo_root / TREND_MD}")
    print(f"Wrote {repo_root / REPLAY_JSON}")
    print(f"Wrote {repo_root / REPLAY_MD}")
    print(f"Wrote {repo_root / BUDGET_JSON}")
    print(f"Wrote {repo_root / BUDGET_MD}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
