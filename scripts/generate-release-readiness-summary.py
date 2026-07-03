#!/usr/bin/env python3
"""Generate a compact public release/parity readiness summary."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def metric(payload: dict[str, Any], *keys: str, default: Any = 0) -> Any:
    cur: Any = payload
    for key in keys:
        if not isinstance(cur, dict):
            return default
        cur = cur.get(key)
    return default if cur is None else cur


def as_int(value: Any) -> int:
    try:
        return int(value or 0)
    except (TypeError, ValueError):
        return 0


def as_bool(value: Any) -> bool:
    return bool(value)


def workspace_version(repo_root: Path) -> str:
    cargo = repo_root / "Cargo.toml"
    text = cargo.read_text(encoding="utf-8") if cargo.exists() else ""
    match = re.search(r"(?m)^version = \"([^\"]+)\"", text)
    return match.group(1) if match else "unknown"


def git_output(repo_root: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def build_summary(repo_root: Path) -> dict[str, Any]:
    proof = load_json(repo_root / "docs/parity/global-parity-proof.json")
    queue = load_json(repo_root / "docs/parity/upstream-missing-queue.json")
    shared = load_json(repo_root / "docs/parity/shared-diff-backlog.json")
    coverage = load_json(repo_root / "docs/parity/test-coverage-audit.json")
    sota = load_json(repo_root / "docs/parity/sota-harness-matrix.json")

    queue_pending = as_int(metric(queue, "summary", "by_disposition", "pending", default=0))
    shared_pending_classification = as_int(metric(shared, "summary", "pending_classification", default=0))
    shared_pending_review = as_int(metric(shared, "summary", "pending_review", default=0))
    coverage_critical = as_int(metric(coverage, "audit_gate", "critical_gaps", default=0))
    coverage_missing_refs = as_int(metric(coverage, "summary", "missing_rust_test_refs", default=0))
    sota_critical = as_int(metric(sota, "gate", "critical_gaps", default=0))
    sota_missing_refs = as_int(metric(sota, "gate", "missing_rust_test_refs", default=0))
    release_gate_pass = as_bool(metric(proof, "release_gate", "pass", default=False))

    checks = [
        {
            "id": "release_gate",
            "label": "Global release gate",
            "actual": "PASS" if release_gate_pass else "FAIL",
            "pass": release_gate_pass,
        },
        {
            "id": "queue_pending",
            "label": "Upstream queue pending",
            "actual": queue_pending,
            "limit": 0,
            "pass": queue_pending == 0,
        },
        {
            "id": "shared_pending_classification",
            "label": "Shared diff pending classification",
            "actual": shared_pending_classification,
            "limit": 0,
            "pass": shared_pending_classification == 0,
        },
        {
            "id": "shared_pending_review",
            "label": "Shared diff pending review",
            "actual": shared_pending_review,
            "limit": 0,
            "pass": shared_pending_review == 0,
        },
        {
            "id": "coverage_critical_gaps",
            "label": "Coverage critical gaps",
            "actual": coverage_critical,
            "limit": 0,
            "pass": coverage_critical == 0,
        },
        {
            "id": "coverage_missing_rust_refs",
            "label": "Coverage missing Rust refs",
            "actual": coverage_missing_refs,
            "limit": 0,
            "pass": coverage_missing_refs == 0,
        },
        {
            "id": "sota_critical_gaps",
            "label": "SOTA harness critical gaps",
            "actual": sota_critical,
            "limit": 0,
            "pass": sota_critical == 0,
        },
        {
            "id": "sota_missing_rust_refs",
            "label": "SOTA harness missing Rust refs",
            "actual": sota_missing_refs,
            "limit": 0,
            "pass": sota_missing_refs == 0,
        },
    ]

    return {
        "schema_version": 1,
        "workspace_version": workspace_version(repo_root),
        "head": git_output(repo_root, "rev-parse", "HEAD"),
        "tag": git_output(repo_root, "describe", "--tags", "--exact-match", "HEAD"),
        "ok": all(bool(check["pass"]) for check in checks),
        "checks": checks,
        "source_artifacts": {
            "global_parity_proof": "docs/parity/global-parity-proof.json",
            "upstream_missing_queue": "docs/parity/upstream-missing-queue.json",
            "shared_diff_backlog": "docs/parity/shared-diff-backlog.json",
            "test_coverage_audit": "docs/parity/test-coverage-audit.json",
            "sota_harness_matrix": "docs/parity/sota-harness-matrix.json",
        },
        "summary": {
            "queue_pending": queue_pending,
            "shared_pending_classification": shared_pending_classification,
            "shared_pending_review": shared_pending_review,
            "coverage_critical_gaps": coverage_critical,
            "coverage_missing_rust_refs": coverage_missing_refs,
            "sota_critical_gaps": sota_critical,
            "sota_missing_rust_refs": sota_missing_refs,
            "release_gate_pass": release_gate_pass,
        },
    }


def render_markdown(summary: dict[str, Any]) -> str:
    status = "PASS" if summary["ok"] else "FAIL"
    lines = [
        "# Hermes Agent Ultra Release Readiness Summary",
        "",
        f"Status: **{status}**",
        f"Workspace version: `{summary['workspace_version']}`",
        f"HEAD: `{str(summary.get('head') or '')[:12]}`",
    ]
    if summary.get("tag"):
        lines.append(f"Tag: `{summary['tag']}`")
    lines.extend([
        "",
        "## Gates",
        "",
        "| Gate | Actual | Limit | Status |",
        "| --- | ---: | ---: | --- |",
    ])
    for check in summary["checks"]:
        limit = check.get("limit", "PASS")
        lines.append(
            f"| {check['label']} | `{check['actual']}` | `{limit}` | "
            f"{'PASS' if check['pass'] else 'FAIL'} |"
        )
    lines.extend([
        "",
        "## Source Artifacts",
        "",
    ])
    for label, path in summary["source_artifacts"].items():
        lines.append(f"- `{label}`: `{path}`")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--output-json", default=".sync-reports/release-readiness-summary.json")
    parser.add_argument("--output-md", default=".sync-reports/release-readiness-summary.md")
    parser.add_argument("--check", action="store_true", help="Exit nonzero if release readiness gates fail")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    summary = build_summary(repo_root)
    output_json = repo_root / args.output_json
    output_md = repo_root / args.output_md
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    output_md.write_text(render_markdown(summary), encoding="utf-8")
    print(f"Wrote {output_json}")
    print(f"Wrote {output_md}")
    print(f"release_readiness={'PASS' if summary['ok'] else 'FAIL'}")
    if args.check and not summary["ok"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
