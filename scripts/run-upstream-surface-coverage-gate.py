#!/usr/bin/env python3
"""Fail when required upstream surface files are missing locally."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import subprocess
from typing import Any


DEFAULT_DIVERGENCE_FILE = "docs/parity/intentional-divergence.json"
DEFAULT_PREFIXES = [
    "skills",
    "optional-skills",
    "plugins",
    "website",
    "ui-tui",
    "docs",
]
RUST_ONLY_DENY_PREFIXES = (
    "tests",
    "tests/",
    "test",
    "test/",
)


def run_git(repo_root: pathlib.Path, args: list[str], check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=str(repo_root),
        text=True,
        capture_output=True,
        check=False,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument("--local-ref", default="HEAD", help="Local ref to validate")
    parser.add_argument(
        "--local-mode",
        choices=["ref", "worktree"],
        default="ref",
        help="Check files against a git ref or current worktree paths",
    )
    parser.add_argument(
        "--upstream-ref",
        default="upstream/main",
        help="Upstream ref to compare against",
    )
    parser.add_argument(
        "--prefix",
        action="append",
        default=[],
        help="Required prefix to validate (repeatable). Defaults to core upstream surfaces.",
    )
    parser.add_argument(
        "--report-path",
        default="",
        help="Optional explicit report output path",
    )
    parser.add_argument(
        "--intentional-divergence",
        default=DEFAULT_DIVERGENCE_FILE,
        help=(
            "Approved divergence registry used to classify non-applicable "
            "missing upstream paths. Pass an empty string to disable."
        ),
    )
    parser.add_argument(
        "--allow-python-test-surfaces",
        action="store_true",
        help=(
            "Allow parity coverage checks over upstream test prefixes. "
            "Default is strict Rust-only parity mode (tests blocked)."
        ),
    )
    parser.add_argument("--json", action="store_true", help="Print JSON report")
    return parser.parse_args()


def path_matches_prefix(path: str, prefix: str) -> bool:
    norm_prefix = normalize_prefix(prefix).rstrip("/")
    if not norm_prefix:
        return False
    return path == norm_prefix or path.startswith(norm_prefix + "/")


def load_divergence_items(
    repo_root: pathlib.Path, divergence_file: str
) -> list[dict[str, Any]]:
    if not divergence_file.strip():
        return []
    div_path = (repo_root / divergence_file).resolve()
    if not div_path.exists():
        return []
    raw = json.loads(div_path.read_text(encoding="utf-8"))
    items = raw.get("items", [])
    if not isinstance(items, list):
        raise RuntimeError(f"invalid intentional divergence payload: {div_path}")
    active_items: list[dict[str, Any]] = []
    for item in items:
        if not isinstance(item, dict):
            continue
        if item.get("status") not in {"approved", "temporary"}:
            continue
        prefixes = item.get("path_prefixes", [])
        if not isinstance(prefixes, list):
            continue
        active_items.append(
            {
                "id": str(item.get("id", "")),
                "status": str(item.get("status", "")),
                "workstream": str(item.get("workstream", "")),
                "path_prefixes": [str(prefix) for prefix in prefixes],
            }
        )
    return active_items


def find_divergence_owner(
    path: str, divergence_items: list[dict[str, Any]]
) -> dict[str, Any] | None:
    for item in divergence_items:
        for prefix in item.get("path_prefixes", []):
            if path_matches_prefix(path, prefix):
                return {
                    "id": item["id"],
                    "status": item["status"],
                    "workstream": item["workstream"],
                    "matched_prefix": normalize_prefix(prefix).rstrip("/"),
                }
    return None


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"upstream-surface-coverage-gate-{stamp}.json"


def list_files(repo_root: pathlib.Path, ref: str, prefix: str) -> list[str]:
    output = run_git(repo_root, ["ls-tree", "-r", "--name-only", ref, prefix], check=False)
    if not output:
        return []
    return [line.strip() for line in output.splitlines() if line.strip()]


def ref_has_path(repo_root: pathlib.Path, ref: str, path: str) -> bool:
    proc = subprocess.run(
        ["git", "cat-file", "-e", f"{ref}:{path}"],
        cwd=str(repo_root),
        text=True,
        capture_output=True,
        check=False,
    )
    return proc.returncode == 0


def normalize_prefix(prefix: str) -> str:
    value = prefix.strip().lstrip("./")
    while "//" in value:
        value = value.replace("//", "/")
    return value


def rust_only_prefix_violations(prefixes: list[str]) -> list[str]:
    violations: list[str] = []
    for raw in prefixes:
        prefix = normalize_prefix(raw)
        if not prefix:
            continue
        for deny in RUST_ONLY_DENY_PREFIXES:
            if prefix == deny or prefix.startswith(deny):
                violations.append(raw)
                break
    return sorted(set(violations))


def build_report(
    repo_root: pathlib.Path,
    local_ref: str,
    local_mode: str,
    upstream_ref: str,
    prefixes: list[str],
    divergence_items: list[dict[str, Any]],
) -> dict[str, Any]:
    by_prefix: dict[str, dict[str, Any]] = {}
    missing_total: list[str] = []
    raw_missing_total: list[str] = []
    divergence_paths: list[dict[str, str]] = []
    divergence_counts: dict[str, int] = {}

    for prefix in prefixes:
        upstream_files = list_files(repo_root, upstream_ref, prefix)
        if local_mode == "worktree":
            raw_missing = [path for path in upstream_files if not (repo_root / path).exists()]
        else:
            raw_missing = [
                path for path in upstream_files if not ref_has_path(repo_root, local_ref, path)
            ]
        actionable_missing: list[str] = []
        diverged_missing: list[dict[str, str]] = []
        for path in raw_missing:
            owner = find_divergence_owner(path, divergence_items)
            if owner is None:
                actionable_missing.append(path)
                continue
            entry = {"path": path, **owner}
            diverged_missing.append(entry)
            divergence_counts[owner["id"]] = divergence_counts.get(owner["id"], 0) + 1

        present = len(upstream_files) - len(raw_missing)
        effective_present = len(upstream_files) - len(actionable_missing)
        coverage = 1.0 if not upstream_files else present / len(upstream_files)
        effective_coverage = (
            1.0 if not upstream_files else effective_present / len(upstream_files)
        )
        by_prefix[prefix] = {
            "upstream_file_count": len(upstream_files),
            "present_locally": present,
            "raw_missing_count": len(raw_missing),
            "intentional_divergence_count": len(diverged_missing),
            "missing_count": len(actionable_missing),
            "coverage_ratio": coverage,
            "effective_coverage_ratio": effective_coverage,
            "missing_sample": actionable_missing[:50],
            "intentional_divergence_sample": diverged_missing[:50],
        }
        raw_missing_total.extend(raw_missing)
        missing_total.extend(actionable_missing)
        divergence_paths.extend(diverged_missing)

    summary = {
        "prefixes_checked": prefixes,
        "upstream_file_count": sum(v["upstream_file_count"] for v in by_prefix.values()),
        "present_locally": sum(v["present_locally"] for v in by_prefix.values()),
        "raw_missing_total": len(raw_missing_total),
        "intentional_divergence_total": len(divergence_paths),
        "missing_total": len(missing_total),
    }

    return {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "local_ref": local_ref,
        "local_mode": local_mode,
        "upstream_ref": upstream_ref,
        "ok": len(missing_total) == 0,
        "summary": summary,
        "by_prefix": by_prefix,
        "missing_paths": sorted(missing_total),
        "intentional_divergence_paths": sorted(
            divergence_paths, key=lambda entry: entry["path"]
        ),
        "intentional_divergence_by_id": dict(sorted(divergence_counts.items())),
    }


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    prefixes = args.prefix if args.prefix else list(DEFAULT_PREFIXES)
    violations = (
        []
        if args.allow_python_test_surfaces
        else rust_only_prefix_violations(prefixes)
    )
    if violations:
        report = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "repo_root": str(repo_root),
            "local_ref": args.local_ref,
            "local_mode": args.local_mode,
            "upstream_ref": args.upstream_ref,
            "ok": False,
            "reason": "rust_only_parity_guard_blocked_prefix",
            "policy": {
                "rust_only": True,
                "allow_python_test_surfaces": bool(args.allow_python_test_surfaces),
                "blocked_prefixes": violations,
            },
        }
        report_path = (
            pathlib.Path(args.report_path).expanduser().resolve()
            if args.report_path
            else default_report_path(repo_root)
        )
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        report["report_path"] = str(report_path)
        if args.json:
            print(json.dumps(report, indent=2))
        else:
            print(
                "[upstream-surface-coverage] FAILED "
                "(rust-only parity guard blocked test prefixes)"
            )
            print(f"[upstream-surface-coverage] Report: {report_path}")
        return 1
    divergence_items = load_divergence_items(repo_root, args.intentional_divergence)
    report = build_report(
        repo_root,
        args.local_ref,
        args.local_mode,
        args.upstream_ref,
        prefixes,
        divergence_items,
    )
    report["policy"] = {
        "rust_only": True,
        "allow_python_test_surfaces": bool(args.allow_python_test_surfaces),
        "blocked_prefixes": [],
        "intentional_divergence": args.intentional_divergence,
        "active_divergence_items": len(divergence_items),
    }

    report_path = (
        pathlib.Path(args.report_path).expanduser().resolve()
        if args.report_path
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    report["report_path"] = str(report_path)

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if report["ok"] else "FAILED"
        print(
            f"[upstream-surface-coverage] {status} "
            f"(missing={report['summary']['missing_total']} "
            f"checked={report['summary']['upstream_file_count']})"
        )
        print(f"[upstream-surface-coverage] Report: {report_path}")
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
