#!/usr/bin/env python3
"""Generate docs/parity/PARITY_DASHBOARD.md from parity JSON artifacts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--parity-matrix",
        default="docs/parity/parity-matrix.json",
        help="Parity matrix JSON path relative to repo root",
    )
    parser.add_argument(
        "--workstream-status",
        default="docs/parity/workstream-status.json",
        help="Workstream status JSON path relative to repo root",
    )
    parser.add_argument(
        "--queue-json",
        default="docs/parity/upstream-missing-queue.json",
        help="Upstream queue JSON path relative to repo root",
    )
    parser.add_argument(
        "--proof-json",
        default="docs/parity/global-parity-proof.json",
        help="Global parity proof JSON path relative to repo root",
    )
    parser.add_argument(
        "--test-coverage-audit",
        default="docs/parity/test-coverage-audit.json",
        help="Test coverage audit JSON path relative to repo root",
    )
    parser.add_argument(
        "--sota-harness-matrix",
        default="docs/parity/sota-harness-matrix.json",
        help="SOTA harness matrix JSON path relative to repo root",
    )
    parser.add_argument(
        "--behavioral-diff",
        default="docs/parity/behavioral-similarity-diff.json",
        help="Behavioral similarity diff JSON path relative to repo root",
    )
    parser.add_argument(
        "--output",
        default="docs/parity/PARITY_DASHBOARD.md",
        help="Output markdown path relative to repo root",
    )
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    return value if isinstance(value, dict) else {}


def gate_status(value: Any) -> str:
    if value is True:
        return "PASS"
    if value is False:
        return "FAIL"
    return "UNKNOWN"


def format_checks_by_status(gate: dict[str, Any], target_status: str) -> str:
    checks = gate.get("checks")
    if not isinstance(checks, list):
        return "none"
    matches: list[str] = []
    for check in checks:
        if not isinstance(check, dict):
            continue
        metric = str(check.get("metric", "unknown_metric"))
        status = str(check.get("status", "unknown")).lower()
        if status == target_status:
            actual = check.get("actual", "n/a")
            limit = check.get("limit", "n/a")
            matches.append(f"{metric} (actual={actual}, limit={limit})")
    if not matches:
        return "none"
    return "; ".join(matches)


def format_failed_checks(gate: dict[str, Any]) -> str:
    return format_checks_by_status(gate, "fail")


def format_warning_checks(gate: dict[str, Any]) -> str:
    return format_checks_by_status(gate, "warn")


def render_dashboard(
    parity_matrix: dict[str, Any],
    workstream_status: dict[str, Any],
    queue_json: dict[str, Any],
    proof_json: dict[str, Any],
    test_coverage_audit: dict[str, Any],
    sota_harness_matrix: dict[str, Any],
    behavioral_diff: dict[str, Any],
) -> str:
    summary = parity_matrix.get("summary", {}) if isinstance(parity_matrix.get("summary"), dict) else {}
    queue_summary = queue_json.get("summary", {}) if isinstance(queue_json.get("summary"), dict) else {}
    by_disp = queue_summary.get("by_disposition", {}) if isinstance(queue_summary.get("by_disposition"), dict) else {}
    ws_states = workstream_status.get("states", {}) if isinstance(workstream_status.get("states"), dict) else {}

    release_gate = proof_json.get("release_gate", {}) if isinstance(proof_json.get("release_gate"), dict) else {}
    ci_gate = proof_json.get("ci_gate", {}) if isinstance(proof_json.get("ci_gate"), dict) else {}
    coverage_summary = (
        test_coverage_audit.get("summary", {})
        if isinstance(test_coverage_audit.get("summary"), dict)
        else {}
    )
    coverage_gate = (
        test_coverage_audit.get("audit_gate", {})
        if isinstance(test_coverage_audit.get("audit_gate"), dict)
        else {}
    )
    harness_summary = (
        sota_harness_matrix.get("summary", {})
        if isinstance(sota_harness_matrix.get("summary"), dict)
        else {}
    )
    harness_gate = (
        sota_harness_matrix.get("gate", {})
        if isinstance(sota_harness_matrix.get("gate"), dict)
        else {}
    )
    behavioral_summary = (
        behavioral_diff.get("summary", {})
        if isinstance(behavioral_diff.get("summary"), dict)
        else {}
    )
    behavioral_gate = (
        behavioral_diff.get("gate", {})
        if isinstance(behavioral_diff.get("gate"), dict)
        else {}
    )

    upstream_ref = str(workstream_status.get("upstream_ref") or "unknown")
    upstream_sha = str(workstream_status.get("upstream_sha") or "unknown")
    ws_generated = str(workstream_status.get("generated_at_utc") or "unknown")
    pm_generated = str(parity_matrix.get("generated_at_utc") or "unknown")
    queue_generated = str(queue_json.get("generated_at_utc") or "unknown")
    proof_generated = str(proof_json.get("generated_at_utc") or "unknown")
    derived_generated = next(
        (
            candidate
            for candidate in [proof_generated, queue_generated, pm_generated, ws_generated]
            if candidate and candidate != "unknown"
        ),
        "unknown",
    )

    lines: list[str] = []
    lines.append("# Parity Dashboard")
    lines.append("")
    lines.append(f"_Generated from source artifacts: `{derived_generated}`_")
    lines.append("")
    lines.append("## Snapshot")
    lines.append("")
    lines.append(f"- Upstream target: `{upstream_ref}` @ `{upstream_sha}`")
    lines.append(f"- Workstream snapshot generated: `{ws_generated}`")
    lines.append(f"- Parity matrix generated: `{pm_generated}`")
    lines.append(f"- Queue snapshot generated: `{queue_generated}`")
    lines.append(f"- Proof snapshot generated: `{proof_generated}`")
    lines.append("")
    lines.append("## Gate Status")
    lines.append("")
    lines.append(f"- Release gate: **{gate_status(release_gate.get('pass'))}**")
    lines.append(f"- CI/tree-drift gate: **{gate_status(ci_gate.get('pass'))}**")
    lines.append(f"- Test coverage audit: **{gate_status(coverage_gate.get('pass'))}**")
    lines.append(f"- SOTA harness matrix: **{gate_status(harness_gate.get('pass'))}**")
    lines.append(f"- Behavioral similarity diff: **{gate_status(behavioral_gate.get('pass'))}**")
    lines.append(f"- Release gate failures: {format_failed_checks(release_gate)}")
    lines.append(f"- CI gate failures: {format_failed_checks(ci_gate)}")
    lines.append(f"- CI gate warnings: {format_warning_checks(ci_gate)}")
    lines.append("")
    lines.append("## Test Coverage Audit")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    lines.append(
        f"| Tracked behavior rows | {int(coverage_summary.get('tracked_behavior_rows', 0) or 0)} |"
    )
    lines.append(
        f"| Covered behavior rows | {int(coverage_summary.get('covered_behavior_rows', 0) or 0)} |"
    )
    lines.append(
        "| Tracked behavior coverage ratio | "
        f"{float(coverage_summary.get('tracked_behavior_coverage_ratio', 0.0) or 0.0):.4f} |"
    )
    lines.append(
        f"| Rust test functions | {int(coverage_summary.get('rust_test_functions', 0) or 0)} |"
    )
    lines.append(
        f"| Missing Rust test refs | {int(coverage_summary.get('missing_rust_test_refs', 0) or 0)} |"
    )
    lines.append(
        f"| Critical gaps | {int(coverage_gate.get('critical_gaps', 0) or 0)} |"
    )
    lines.append("")
    lines.append("## SOTA Harness Matrix")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    lines.append(f"| Domains total | {int(harness_summary.get('domains_total', 0) or 0)} |")
    lines.append(f"| Domains passing | {int(harness_summary.get('domains_passing', 0) or 0)} |")
    lines.append(
        "| Domain coverage ratio | "
        f"{float(harness_summary.get('domain_coverage_ratio', 0.0) or 0.0):.4f} |"
    )
    lines.append(f"| Direct Rust tests | {int(harness_summary.get('direct_rust_tests', 0) or 0)} |")
    lines.append(
        f"| Critical gaps | {int(harness_gate.get('critical_gaps', 0) or 0)} |"
    )
    lines.append(
        f"| Missing Rust test refs | {int(harness_gate.get('missing_rust_test_refs', 0) or 0)} |"
    )
    lines.append("")
    lines.append("## Behavioral Similarity Diff")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    lines.append(f"| Total cases | {int(behavioral_summary.get('total_cases', 0) or 0)} |")
    lines.append(
        f"| Equal or better cases | {int(behavioral_summary.get('equal_or_better_cases', 0) or 0)} |"
    )
    lines.append(f"| Superior cases | {int(behavioral_summary.get('superior_cases', 0) or 0)} |")
    lines.append(
        "| Similarity ratio | "
        f"{float(behavioral_summary.get('behavioral_similarity_ratio', 0.0) or 0.0):.4f} |"
    )
    lines.append(f"| Regressions | {int(behavioral_summary.get('regressions', 0) or 0)} |")
    lines.append(f"| Gaps | {int(behavioral_summary.get('gaps', 0) or 0)} |")
    lines.append(f"| Unverified cases | {int(behavioral_summary.get('unverified_cases', 0) or 0)} |")
    lines.append(
        f"| Missing Rust test refs | {int(behavioral_summary.get('missing_rust_test_refs', 0) or 0)} |"
    )
    lines.append("")
    lines.append("## Queue Summary")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    lines.append(f"| Total commits in queue | {int(queue_summary.get('total_commits', 0) or 0)} |")
    lines.append(f"| Pending | {int(by_disp.get('pending', 0) or 0)} |")
    lines.append(f"| Ported | {int(by_disp.get('ported', 0) or 0)} |")
    lines.append(f"| Superseded | {int(by_disp.get('superseded', 0) or 0)} |")
    lines.append("")
    lines.append("## Tree/Patch Drift")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    for key in [
        "commits_behind",
        "commits_ahead",
        "upstream_patch_missing",
        "upstream_patch_represented",
        "local_patch_unique",
        "files_only_upstream",
        "files_only_local",
        "files_shared_identical",
        "files_shared_different",
    ]:
        lines.append(f"| {key} | {int(summary.get(key, 0) or 0)} |")
    lines.append("")
    lines.append("## Workstream States")
    lines.append("")
    lines.append("| State | Count |")
    lines.append("| --- | ---: |")
    for state_name in sorted(ws_states):
        lines.append(f"| {state_name} | {int(ws_states[state_name] or 0)} |")
    lines.append("")

    workstreams = workstream_status.get("workstreams")
    if isinstance(workstreams, list) and workstreams:
        lines.append("## Workstream Detail")
        lines.append("")
        lines.append("| WS | Title | State |")
        lines.append("| --- | --- | --- |")
        for ws in workstreams:
            if not isinstance(ws, dict):
                continue
            ws_id = str(ws.get("workstream", "?"))
            title = str(ws.get("title", ""))
            state = str(ws.get("state", ""))
            lines.append(f"| {ws_id} | {title} | {state} |")
        lines.append("")

    lines.append("## Source Artifacts")
    lines.append("")
    lines.append("- `docs/parity/parity-matrix.json`")
    lines.append("- `docs/parity/workstream-status.json`")
    lines.append("- `docs/parity/upstream-missing-queue.json`")
    lines.append("- `docs/parity/global-parity-proof.json`")
    lines.append("- `docs/parity/test-coverage-audit.json`")
    lines.append("- `docs/parity/sota-harness-matrix.json`")
    lines.append("- `docs/parity/behavioral-similarity-diff.json`")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).expanduser().resolve()

    parity_matrix = load_json((repo_root / args.parity_matrix).resolve())
    workstream_status = load_json((repo_root / args.workstream_status).resolve())
    queue_json = load_json((repo_root / args.queue_json).resolve())
    proof_json = load_json((repo_root / args.proof_json).resolve())
    test_coverage_audit = load_json((repo_root / args.test_coverage_audit).resolve())
    sota_harness_matrix = load_json((repo_root / args.sota_harness_matrix).resolve())
    behavioral_diff = load_json((repo_root / args.behavioral_diff).resolve())

    output = (repo_root / args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    dashboard = render_dashboard(
        parity_matrix,
        workstream_status,
        queue_json,
        proof_json,
        test_coverage_audit,
        sota_harness_matrix,
        behavioral_diff,
    )
    output.write_text(dashboard + "\n", encoding="utf-8")
    print(f"Wrote parity dashboard: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
