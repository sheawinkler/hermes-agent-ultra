#!/usr/bin/env python3
"""Generate outcome-level upstream-vs-Ultra behavioral similarity diff artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from collections import Counter
from pathlib import Path
from typing import Any


RUST_TEST_ATTR_RE = re.compile(r"#\[\s*(?:tokio::)?test(?:\([^]]*\))?\s*\]")
RUST_FN_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
PASSING_CLASSIFICATIONS = {"equivalent", "superior", "intentional_divergence"}
FAILING_CLASSIFICATIONS = {"regression", "gap"}
ALLOWED_CLASSIFICATIONS = PASSING_CLASSIFICATIONS | FAILING_CLASSIFICATIONS | {"unverified"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--cases",
        default="docs/parity/behavioral-similarity-cases.json",
        type=Path,
        help="Behavioral similarity case manifest",
    )
    parser.add_argument(
        "--out-json",
        default="docs/parity/behavioral-similarity-diff.json",
        type=Path,
        help="Output JSON path relative to repo root",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/behavioral-similarity-diff.md",
        type=Path,
        help="Output Markdown path relative to repo root",
    )
    parser.add_argument(
        "--min-superiority-cases",
        default=5,
        type=int,
        help="Minimum strictly-superior cases required for the behavioral gate",
    )
    parser.add_argument("--check", action="store_true", help="Fail when behavioral gate fails")
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def rel(path: Path, repo_root: Path) -> str:
    return path.relative_to(repo_root).as_posix()


def has_attached_test_attr(text: str, fn_start: int) -> bool:
    line_start = text.rfind("\n", 0, fn_start)
    prefix_lines = text[:line_start].splitlines() if line_start >= 0 else []
    for line in reversed(prefix_lines):
        stripped = line.strip()
        if not stripped:
            break
        if not stripped.startswith("#["):
            break
        if RUST_TEST_ATTR_RE.fullmatch(stripped):
            return True
    return False


def scan_rust_tests(repo_root: Path) -> dict[str, set[str]]:
    tests_by_file: dict[str, set[str]] = {}
    candidates = sorted((repo_root / "crates").glob("**/*.rs")) + sorted(
        (repo_root / "tests").glob("**/*.rs")
    )
    for path in candidates:
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        names: set[str] = set()
        for match in RUST_FN_RE.finditer(text):
            if has_attached_test_attr(text, match.start()):
                names.add(match.group(1))
        if names:
            tests_by_file[rel(path, repo_root)] = names
    return tests_by_file


def rust_test_exists(tests_by_file: dict[str, set[str]], file: str, name: str) -> bool:
    return name in tests_by_file.get(file, set())


def as_list(value: Any) -> list[Any]:
    return value if isinstance(value, list) else []


def case_issue(case_id: str, kind: str, message: str, **extra: Any) -> dict[str, Any]:
    payload: dict[str, Any] = {"case_id": case_id, "kind": kind, "message": message}
    payload.update(extra)
    return payload


def validate_case(
    repo_root: Path,
    tests_by_file: dict[str, set[str]],
    case: dict[str, Any],
) -> dict[str, Any]:
    case_id = str(case.get("id", "")).strip()
    classification = str(case.get("classification", "")).strip()
    issues: list[dict[str, Any]] = []

    if not case_id:
        issues.append(case_issue("<missing-id>", "schema", "case id is required"))
    if classification not in ALLOWED_CLASSIFICATIONS:
        issues.append(
            case_issue(
                case_id or "<missing-id>",
                "schema",
                "classification is not allowed",
                classification=classification,
            )
        )

    source_artifacts = [str(path).strip() for path in as_list(case.get("source_artifacts")) if str(path).strip()]
    if not source_artifacts:
        issues.append(case_issue(case_id, "missing_source_artifacts", "case has no source artifacts"))
    missing_artifacts = [path for path in source_artifacts if not (repo_root / path).exists()]
    for path in missing_artifacts:
        issues.append(
            case_issue(case_id, "missing_source_artifact", "source artifact does not exist", path=path)
        )

    rust_tests = [test for test in as_list(case.get("rust_tests")) if isinstance(test, dict)]
    if not rust_tests:
        issues.append(case_issue(case_id, "missing_rust_tests", "case has no Rust test references"))

    missing_rust_refs: list[dict[str, str]] = []
    for test in rust_tests:
        file = str(test.get("file", "")).strip()
        name = str(test.get("name", "")).strip()
        if not file or not name or not rust_test_exists(tests_by_file, file, name):
            missing_rust_refs.append({"file": file, "name": name})
            issues.append(
                case_issue(
                    case_id,
                    "missing_rust_test_ref",
                    "referenced Rust test was not found",
                    file=file,
                    name=name,
                )
            )

    text_fields = ["domain", "upstream_behavior", "ultra_behavior", "why"]
    for field in text_fields:
        if not str(case.get(field, "")).strip():
            issues.append(case_issue(case_id, "schema", f"{field} is required"))

    score = 1.0 if classification in PASSING_CLASSIFICATIONS else 0.0
    row = dict(case)
    row["score"] = score
    row["issues"] = issues
    row["missing_rust_test_refs"] = missing_rust_refs
    row["missing_source_artifacts"] = missing_artifacts
    row["verified"] = not issues and classification != "unverified"
    return row


def render_md(payload: dict[str, Any]) -> str:
    summary = payload["summary"]
    gate = payload["gate"]
    lines: list[str] = []
    lines.append("# Behavioral Similarity Diff")
    lines.append("")
    lines.append(f"Generated: `{payload['generated_at_utc']}`")
    lines.append("")
    lines.append("## Gate")
    lines.append("")
    lines.append(f"- Gate: **{'PASS' if gate['pass'] else 'FAIL'}**")
    lines.append(f"- Similarity ratio: `{summary['behavioral_similarity_ratio']}`")
    lines.append(f"- Superior cases: `{summary['superior_cases']}`")
    lines.append(f"- Regressions: `{summary['regressions']}`")
    lines.append(f"- Gaps: `{summary['gaps']}`")
    lines.append(f"- Unverified cases: `{summary['unverified_cases']}`")
    lines.append(f"- Missing Rust test refs: `{summary['missing_rust_test_refs']}`")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append("| Metric | Value |")
    lines.append("| --- | ---: |")
    for key in [
        "total_cases",
        "equal_or_better_cases",
        "superior_cases",
        "equivalent_cases",
        "intentional_divergence_cases",
        "regressions",
        "gaps",
        "unverified_cases",
        "missing_rust_test_refs",
        "missing_source_artifacts",
        "behavioral_similarity_ratio",
    ]:
        lines.append(f"| `{key}` | {summary.get(key, 0)} |")
    lines.append("")
    lines.append("## Classification Counts")
    lines.append("")
    lines.append("| Classification | Count |")
    lines.append("| --- | ---: |")
    for classification, count in sorted(summary["classification_counts"].items()):
        lines.append(f"| `{classification}` | {count} |")
    lines.append("")
    lines.append("## Domain Counts")
    lines.append("")
    lines.append("| Domain | Count |")
    lines.append("| --- | ---: |")
    for domain, count in sorted(summary["domain_counts"].items()):
        lines.append(f"| `{domain}` | {count} |")
    lines.append("")
    lines.append("## Cases")
    lines.append("")
    lines.append("| Case | Domain | Classification | Score |")
    lines.append("| --- | --- | --- | ---: |")
    for case in payload["cases"]:
        lines.append(
            f"| `{case['id']}` | `{case['domain']}` | `{case['classification']}` | {case['score']} |"
        )
    lines.append("")
    lines.append("## Gate Failures")
    lines.append("")
    if payload["gate_failures"]:
        for failure in payload["gate_failures"]:
            lines.append(f"- `{failure['kind']}`: {failure['message']}")
    else:
        lines.append("- none")
    lines.append("")
    lines.append("## Case Issues")
    lines.append("")
    case_issues = [issue for case in payload["cases"] for issue in case["issues"]]
    if case_issues:
        for issue in case_issues:
            lines.append(f"- `{issue['case_id']}` `{issue['kind']}`: {issue['message']}")
    else:
        lines.append("- none")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).resolve()
    manifest_path = (repo_root / args.cases).resolve()
    manifest = load_json(manifest_path)
    raw_cases = [case for case in as_list(manifest.get("cases")) if isinstance(case, dict)]
    tests_by_file = scan_rust_tests(repo_root)
    cases = [validate_case(repo_root, tests_by_file, case) for case in raw_cases]

    total = len(cases)
    classification_counts: Counter[str] = Counter(str(case.get("classification", "")) for case in cases)
    domain_counts: Counter[str] = Counter(str(case.get("domain", "")) for case in cases)
    equal_or_better = sum(1 for case in cases if case["classification"] in PASSING_CLASSIFICATIONS)
    superior = classification_counts.get("superior", 0)
    equivalent = classification_counts.get("equivalent", 0)
    intentional = classification_counts.get("intentional_divergence", 0)
    regressions = classification_counts.get("regression", 0)
    gaps = classification_counts.get("gap", 0)
    unverified = classification_counts.get("unverified", 0)
    missing_rust_refs = sum(len(case["missing_rust_test_refs"]) for case in cases)
    missing_artifacts = sum(len(case["missing_source_artifacts"]) for case in cases)
    similarity_ratio = round(equal_or_better / total, 4) if total else 0.0

    gate_failures: list[dict[str, Any]] = []
    if not total:
        gate_failures.append({"kind": "empty_cases", "message": "behavioral case manifest is empty"})
    if similarity_ratio < 1.0:
        gate_failures.append(
            {
                "kind": "similarity_ratio",
                "message": "behavioral similarity ratio is below 1.0",
                "actual": similarity_ratio,
                "limit": 1.0,
            }
        )
    if superior < args.min_superiority_cases:
        gate_failures.append(
            {
                "kind": "superiority_cases",
                "message": "strictly superior behavioral cases are below threshold",
                "actual": superior,
                "limit": args.min_superiority_cases,
            }
        )
    if regressions:
        gate_failures.append(
            {"kind": "regressions", "message": "behavioral regressions are present", "actual": regressions, "limit": 0}
        )
    if gaps:
        gate_failures.append(
            {"kind": "gaps", "message": "behavioral gaps are present", "actual": gaps, "limit": 0}
        )
    if unverified:
        gate_failures.append(
            {"kind": "unverified", "message": "unverified behavioral cases are present", "actual": unverified, "limit": 0}
        )
    if missing_rust_refs:
        gate_failures.append(
            {
                "kind": "missing_rust_test_refs",
                "message": "behavioral cases reference missing Rust tests",
                "actual": missing_rust_refs,
                "limit": 0,
            }
        )
    if missing_artifacts:
        gate_failures.append(
            {
                "kind": "missing_source_artifacts",
                "message": "behavioral cases reference missing source artifacts",
                "actual": missing_artifacts,
                "limit": 0,
            }
        )

    summary = {
        "total_cases": total,
        "equal_or_better_cases": equal_or_better,
        "superior_cases": superior,
        "equivalent_cases": equivalent,
        "intentional_divergence_cases": intentional,
        "regressions": regressions,
        "gaps": gaps,
        "unverified_cases": unverified,
        "missing_rust_test_refs": missing_rust_refs,
        "missing_source_artifacts": missing_artifacts,
        "behavioral_similarity_ratio": similarity_ratio,
        "classification_counts": dict(sorted(classification_counts.items())),
        "domain_counts": dict(sorted(domain_counts.items())),
        "min_superiority_cases": args.min_superiority_cases,
    }
    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "schema_version": 1,
        "manifest": args.cases.as_posix(),
        "summary": summary,
        "gate": {
            "pass": not gate_failures,
            "failures": len(gate_failures),
            "min_similarity_ratio": 1.0,
            "min_superiority_cases": args.min_superiority_cases,
            "max_regressions": 0,
            "max_gaps": 0,
            "max_unverified_cases": 0,
            "max_missing_rust_test_refs": 0,
        },
        "gate_failures": gate_failures,
        "cases": cases,
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    write_json(out_json, payload)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_md.write_text(render_md(payload) + "\n", encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    if args.check and not payload["gate"]["pass"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
