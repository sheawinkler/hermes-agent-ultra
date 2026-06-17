#!/usr/bin/env python3
"""Generate an upstream-vs-Ultra behavior test coverage audit."""

from __future__ import annotations

import argparse
import datetime as dt
import importlib.util
import json
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Any


RUST_TEST_ATTR_RE = re.compile(r"#\[\s*(?:tokio::)?test(?:\([^]]*\))?\s*\]")
RUST_FN_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
HARNESS_HARDENING_MOVES = [
    {
        "title": "Coverage trend ledger",
        "rationale": "Track behavior coverage over time so new upstream rows or local harness regressions create visible deltas before release prep.",
        "artifacts": [
            "docs/parity/harness-trend-ledger.json",
            "docs/parity/harness-trend-ledger.md",
        ],
    },
    {
        "title": "ContextLattice replay evidence index",
        "rationale": "Index passing and failing replay artifacts into ContextLattice so agents can retrieve exact harness evidence instead of rediscovering it from scratch.",
        "artifacts": [
            "docs/parity/contextlattice-replay-evidence-index.json",
            "docs/parity/contextlattice-replay-evidence-index.md",
        ],
    },
    {
        "title": "Cross-version harness budget",
        "rationale": "Record runtime and fixture-count budgets across releases so SOTA harness growth stays deterministic, bounded, and reviewable.",
        "artifacts": [
            "docs/parity/harness-budget.json",
            "docs/parity/harness-budget.md",
        ],
    },
]


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def rel(path: Path, repo_root: Path) -> str:
    return path.relative_to(repo_root).as_posix()


def scan_rust_tests(repo_root: Path) -> dict[str, set[str]]:
    tests_by_file: dict[str, set[str]] = {}
    for path in sorted((repo_root / "crates").glob("**/*.rs")) + sorted(
        (repo_root / "tests").glob("**/*.rs")
    ):
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        names: set[str] = set()
        for match in RUST_FN_RE.finditer(text):
            prefix = text[max(0, match.start() - 700) : match.start()]
            if RUST_TEST_ATTR_RE.search(prefix):
                names.add(match.group(1))
        if names:
            tests_by_file[rel(path, repo_root)] = names
    return tests_by_file


def rust_test_exists(tests_by_file: dict[str, set[str]], file: str, name: str) -> bool:
    return name in tests_by_file.get(file, set())


def load_intent_specs(repo_root: Path) -> list[dict[str, Any]]:
    script = repo_root / "scripts" / "generate-test-intent-mapping.py"
    spec = importlib.util.spec_from_file_location("generate_test_intent_mapping", script)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed loading {script}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)

    rows: list[dict[str, Any]] = []
    for intent in module.INTENTS:
        evidence = module.collect_matches(repo_root, intent.local_evidence_globs)
        rows.append(
            {
                "id": intent.id,
                "upstream_scope": list(intent.upstream_scope),
                "local_evidence_globs": list(intent.local_evidence_globs),
                "evidence": evidence,
            }
        )
    return rows


def evidence_has_direct_test(path: str, tests_by_file: dict[str, set[str]]) -> bool:
    if path in tests_by_file and tests_by_file[path]:
        return True
    if path.startswith("tests/"):
        return True
    if "/tests/" in path:
        return True
    if "/fixtures/" in path or path.startswith("crates/hermes-parity-tests/fixtures/"):
        return True
    return False


def load_coverage_manifest(repo_root: Path, path: str) -> dict[str, Any]:
    full = repo_root / path
    payload = load_json(full)
    entries = payload.get("entries")
    if not isinstance(entries, list):
        entries = []
    return {
        "path": path,
        "summary": payload.get("summary", {}),
        "entries": entries,
    }


def validate_manifest_refs(
    manifest: dict[str, Any],
    tests_by_file: dict[str, set[str]],
) -> dict[str, Any]:
    missing_test_arrays: list[str] = []
    missing_refs: list[dict[str, str]] = []
    status_counts: Counter[str] = Counter()
    referenced_tests: set[tuple[str, str]] = set()

    for entry in manifest["entries"]:
        if not isinstance(entry, dict):
            continue
        path = str(entry.get("path", "unknown"))
        status_counts[str(entry.get("status", "unknown"))] += 1
        rust_tests = entry.get("rust_tests")
        if not isinstance(rust_tests, list) or not rust_tests:
            missing_test_arrays.append(path)
            continue
        for test in rust_tests:
            if not isinstance(test, dict):
                missing_refs.append({"path": path, "file": "unknown", "name": "unknown"})
                continue
            file = str(test.get("file", ""))
            name = str(test.get("name", ""))
            if file and name:
                referenced_tests.add((file, name))
            if not file or not name or not rust_test_exists(tests_by_file, file, name):
                missing_refs.append({"path": path, "file": file, "name": name})

    valid_entries = len(manifest["entries"]) - len(missing_test_arrays)
    return {
        "path": manifest["path"],
        "entries": len(manifest["entries"]),
        "valid_entries_with_rust_tests": valid_entries,
        "missing_rust_tests_entries": missing_test_arrays,
        "missing_rust_test_refs": missing_refs,
        "referenced_rust_tests": len(referenced_tests),
        "status_counts": dict(sorted(status_counts.items())),
    }


def critical_gap(kind: str, message: str, **extra: Any) -> dict[str, Any]:
    payload: dict[str, Any] = {"kind": kind, "message": message}
    payload.update(extra)
    return payload


def advisory(kind: str, message: str, **extra: Any) -> dict[str, Any]:
    payload: dict[str, Any] = {"kind": kind, "message": message}
    payload.update(extra)
    return payload


def render_md(payload: dict[str, Any]) -> str:
    summary = payload["summary"]
    gate = payload["audit_gate"]
    lines: list[str] = []
    lines.append("# Test Coverage Audit")
    lines.append("")
    lines.append(f"Generated: `{payload['generated_at_utc']}`")
    lines.append("")
    lines.append("## Gate")
    lines.append("")
    lines.append(f"- Audit gate: **{'PASS' if gate['pass'] else 'FAIL'}**")
    lines.append(f"- Critical gaps: `{gate['critical_gaps']}`")
    lines.append(f"- Advisory gaps: `{gate['advisory_gaps']}`")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    for key in [
        "tracked_behavior_rows",
        "covered_behavior_rows",
        "tracked_behavior_coverage_ratio",
        "rust_test_files",
        "rust_test_functions",
        "coverage_manifest_entries",
        "coverage_manifest_entries_with_valid_rust_tests",
        "missing_rust_test_refs",
        "queue_pending",
        "queue_total",
        "test_intents_total",
        "test_intents_mapped",
    ]:
        lines.append(f"| `{key}` | {summary.get(key, 0)} |")
    lines.append("")
    lines.append("## Coverage Manifests")
    lines.append("")
    lines.append("| Manifest | Entries | Valid Rust-test entries | Referenced Rust tests | Missing refs |")
    lines.append("| --- | ---: | ---: | ---: | ---: |")
    for manifest in payload["coverage_manifests"]:
        lines.append(
            f"| `{manifest['path']}` | {manifest['entries']} | "
            f"{manifest['valid_entries_with_rust_tests']} | "
            f"{manifest['referenced_rust_tests']} | "
            f"{len(manifest['missing_rust_test_refs'])} |"
        )
    lines.append("")
    lines.append("## Test Intent Domains")
    lines.append("")
    lines.append("| Intent | Classification | Evidence files | Direct test evidence |")
    lines.append("| --- | --- | ---: | ---: |")
    for row in payload["intent_domains"]:
        lines.append(
            f"| `{row['id']}` | `{row['classification']}` | "
            f"{row['evidence_count']} | {row['direct_test_evidence_count']} |"
        )
    lines.append("")
    lines.append("## Critical Gaps")
    lines.append("")
    if payload["critical_gaps"]:
        for gap in payload["critical_gaps"]:
            lines.append(f"- `{gap['kind']}`: {gap['message']}")
    else:
        lines.append("- none")
    lines.append("")
    lines.append("## Advisory Gaps")
    lines.append("")
    if payload["advisory_gaps"]:
        for gap in payload["advisory_gaps"]:
            lines.append(f"- `{gap['kind']}`: {gap['message']}")
    else:
        lines.append("- none")
    lines.append("")
    lines.append("## Completed Sigma Harness Moves")
    lines.append("")
    completed_moves = payload.get("completed_sigma_harness_moves", [])
    if completed_moves:
        for item in completed_moves:
            artifacts = ", ".join(f"`{path}`" for path in item["artifacts"])
            lines.append(f"- **{item['title']}**: {artifacts}")
    else:
        lines.append("- none")
    lines.append("")
    lines.append("## Next Sigma Harness Moves")
    lines.append("")
    next_moves = payload["next_sigma_harness_moves"]
    if next_moves:
        for item in next_moves:
            lines.append(f"- **{item['title']}**: {item['rationale']}")
    else:
        lines.append("- none")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--out-json",
        default="docs/parity/test-coverage-audit.json",
        type=Path,
        help="Output JSON path relative to repo root",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/test-coverage-audit.md",
        type=Path,
        help="Output Markdown path relative to repo root",
    )
    parser.add_argument("--check", action="store_true", help="Fail when audit gate fails")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    tests_by_file = scan_rust_tests(repo_root)
    rust_test_functions = sum(len(names) for names in tests_by_file.values())

    intent_mapping = load_json(repo_root / "docs/parity/test-intent-mapping.json")
    parity_matrix = load_json(repo_root / "docs/parity/parity-matrix.json")
    patch_queue = load_json(repo_root / "docs/parity/upstream-missing-queue.json")
    divergence_validation = load_json(repo_root / "docs/parity/divergence-validation.json")

    intent_specs = load_intent_specs(repo_root)
    intent_rows: list[dict[str, Any]] = []
    for intent in intent_specs:
        evidence = list(intent["evidence"])
        direct_test_evidence = [
            path for path in evidence if evidence_has_direct_test(path, tests_by_file)
        ]
        mapped = bool(evidence)
        classification = "direct_rust_test" if direct_test_evidence else "domain_contract"
        if not mapped:
            classification = "missing"
        intent_rows.append(
            {
                "id": intent["id"],
                "classification": classification,
                "mapped": mapped,
                "upstream_scope": intent["upstream_scope"],
                "evidence_count": len(evidence),
                "direct_test_evidence_count": len(direct_test_evidence),
                "evidence_sample": evidence[:25],
                "direct_test_evidence_sample": direct_test_evidence[:25],
            }
        )

    manifest_paths = [
        "docs/parity/python-test-suite-coverage.json",
        "docs/parity/hermes-cli-test-coverage.json",
        "docs/parity/ui-tui-source-coverage.json",
    ]
    manifest_rows = [
        validate_manifest_refs(load_coverage_manifest(repo_root, path), tests_by_file)
        for path in manifest_paths
    ]

    critical_gaps: list[dict[str, Any]] = []
    advisory_gaps: list[dict[str, Any]] = []

    queue_summary = patch_queue.get("summary", {})
    by_disposition = queue_summary.get("by_disposition", {})
    queue_pending = int(by_disposition.get("pending", 0) or 0)
    if queue_pending:
        critical_gaps.append(
            critical_gap("upstream_queue_pending", "upstream missing queue has pending rows", pending=queue_pending)
        )

    intent_summary = intent_mapping.get("summary", {})
    unmapped_intents = int(intent_summary.get("unmapped_intents", 0) or 0)
    if unmapped_intents:
        critical_gaps.append(
            critical_gap("unmapped_test_intents", "test-intent mapping has unmapped domains", unmapped=unmapped_intents)
        )

    divergence_summary = divergence_validation.get("summary", {})
    for key in ["errors", "unowned", "review_overdue"]:
        value = int(divergence_summary.get(key, 0) or 0)
        if value:
            critical_gaps.append(
                critical_gap("divergence_governance", f"divergence validation has {key}", count=value)
            )

    for manifest in manifest_rows:
        if manifest["missing_rust_tests_entries"]:
            critical_gaps.append(
                critical_gap(
                    "coverage_manifest_missing_tests",
                    f"{manifest['path']} has entries without rust_tests",
                    count=len(manifest["missing_rust_tests_entries"]),
                )
            )
        if manifest["missing_rust_test_refs"]:
            critical_gaps.append(
                critical_gap(
                    "coverage_manifest_missing_refs",
                    f"{manifest['path']} references missing Rust test functions",
                    count=len(manifest["missing_rust_test_refs"]),
                )
            )

    missing_domain_contracts = [row["id"] for row in intent_rows if row["classification"] == "missing"]
    if missing_domain_contracts:
        critical_gaps.append(
            critical_gap(
                "missing_domain_contracts",
                "test-intent domains have no local evidence",
                ids=missing_domain_contracts,
            )
        )

    source_only_domains = [
        row["id"]
        for row in intent_rows
        if row["classification"] == "domain_contract" and row["direct_test_evidence_count"] == 0
    ]
    if source_only_domains:
        advisory_gaps.append(
            advisory(
                "domain_contract_without_direct_test_file",
                "some mapped intent domains are backed by source contracts without direct test-file evidence in the intent map",
                ids=source_only_domains,
            )
        )

    parity_summary = parity_matrix.get("summary", {})
    for source_key, metric_name in [
        ("commits_behind", "max_commits_behind"),
        ("upstream_patch_missing", "max_upstream_patch_missing"),
        ("files_only_upstream", "max_files_only_upstream"),
    ]:
        value = float(parity_summary.get(source_key, 0.0) or 0.0)
        if value > 0:
            advisory_gaps.append(
                advisory(
                    "nonzero_tree_drift",
                    f"{metric_name} remains nonzero in parity matrix",
                    metric=metric_name,
                    value=value,
                )
            )

    manifest_entries = sum(int(row["entries"]) for row in manifest_rows)
    manifest_entries_valid = sum(int(row["valid_entries_with_rust_tests"]) for row in manifest_rows)
    missing_refs = sum(len(row["missing_rust_test_refs"]) for row in manifest_rows)
    tracked_rows = len(intent_rows) + manifest_entries
    covered_rows = sum(1 for row in intent_rows if row["mapped"]) + manifest_entries_valid
    coverage_ratio = covered_rows / tracked_rows if tracked_rows else 0.0

    summary = {
        "tracked_behavior_rows": tracked_rows,
        "covered_behavior_rows": covered_rows,
        "tracked_behavior_coverage_ratio": round(coverage_ratio, 4),
        "rust_test_files": len(tests_by_file),
        "rust_test_functions": rust_test_functions,
        "coverage_manifest_entries": manifest_entries,
        "coverage_manifest_entries_with_valid_rust_tests": manifest_entries_valid,
        "missing_rust_test_refs": missing_refs,
        "queue_pending": queue_pending,
        "queue_total": int(queue_summary.get("total_commits", 0) or 0),
        "test_intents_total": int(intent_summary.get("total_intents", 0) or 0),
        "test_intents_mapped": int(intent_summary.get("mapped_intents", 0) or 0),
        "test_intent_mapping_ratio": float(intent_summary.get("mapping_ratio", 0.0) or 0.0),
        "divergence_errors": int(divergence_summary.get("errors", 0) or 0),
        "divergence_unowned": int(divergence_summary.get("unowned", 0) or 0),
        "divergence_review_overdue": int(divergence_summary.get("review_overdue", 0) or 0),
    }

    hardening_moves: list[dict[str, Any]] = []
    for move in HARNESS_HARDENING_MOVES:
        artifacts = list(move["artifacts"])
        complete = all((repo_root / artifact).exists() for artifact in artifacts)
        row = {
            "title": move["title"],
            "rationale": move["rationale"],
            "artifacts": artifacts,
            "status": "complete" if complete else "pending",
        }
        hardening_moves.append(row)

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "schema_version": 1,
        "audit_unit": "upstream_behavior_or_test_intent",
        "summary": summary,
        "audit_gate": {
            "pass": not critical_gaps,
            "critical_gaps": len(critical_gaps),
            "advisory_gaps": len(advisory_gaps),
        },
        "source_artifacts": {
            "test_intent_mapping": "docs/parity/test-intent-mapping.json",
            "parity_matrix": "docs/parity/parity-matrix.json",
            "upstream_missing_queue": "docs/parity/upstream-missing-queue.json",
            "divergence_validation": "docs/parity/divergence-validation.json",
            "coverage_manifests": manifest_paths,
        },
        "intent_domains": intent_rows,
        "coverage_manifests": manifest_rows,
        "critical_gaps": critical_gaps,
        "advisory_gaps": advisory_gaps,
        "completed_sigma_harness_moves": [
            row for row in hardening_moves if row["status"] == "complete"
        ],
        "next_sigma_harness_moves": [
            row for row in hardening_moves if row["status"] == "pending"
        ],
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    write_json(out_json, payload)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_md.write_text(render_md(payload), encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")

    if args.check and not payload["audit_gate"]["pass"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
