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


def format_failed_checks(gate: dict[str, Any]) -> str:
    checks = gate.get("checks")
    if not isinstance(checks, list):
        return "none"
    failed: list[str] = []
    for check in checks:
        if not isinstance(check, dict):
            continue
        metric = str(check.get("metric", "unknown_metric"))
        status = str(check.get("status", "unknown")).lower()
        if status != "pass":
            actual = check.get("actual", "n/a")
            limit = check.get("limit", "n/a")
            failed.append(f"{metric} (actual={actual}, limit={limit})")
    if not failed:
        return "none"
    return "; ".join(failed)


def render_dashboard(
    parity_matrix: dict[str, Any],
    workstream_status: dict[str, Any],
    queue_json: dict[str, Any],
    proof_json: dict[str, Any],
) -> str:
    summary = parity_matrix.get("summary", {}) if isinstance(parity_matrix.get("summary"), dict) else {}
    queue_summary = queue_json.get("summary", {}) if isinstance(queue_json.get("summary"), dict) else {}
    by_disp = queue_summary.get("by_disposition", {}) if isinstance(queue_summary.get("by_disposition"), dict) else {}
    ws_states = workstream_status.get("states", {}) if isinstance(workstream_status.get("states"), dict) else {}

    release_gate = proof_json.get("release_gate", {}) if isinstance(proof_json.get("release_gate"), dict) else {}
    ci_gate = proof_json.get("ci_gate", {}) if isinstance(proof_json.get("ci_gate"), dict) else {}

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
    lines.append(f"- Release gate failures: {format_failed_checks(release_gate)}")
    lines.append(f"- CI gate failures: {format_failed_checks(ci_gate)}")
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
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).expanduser().resolve()

    parity_matrix = load_json((repo_root / args.parity_matrix).resolve())
    workstream_status = load_json((repo_root / args.workstream_status).resolve())
    queue_json = load_json((repo_root / args.queue_json).resolve())
    proof_json = load_json((repo_root / args.proof_json).resolve())

    output = (repo_root / args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    dashboard = render_dashboard(parity_matrix, workstream_status, queue_json, proof_json)
    output.write_text(dashboard + "\n", encoding="utf-8")
    print(f"Wrote parity dashboard: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
