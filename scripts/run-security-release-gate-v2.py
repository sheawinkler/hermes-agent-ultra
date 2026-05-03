#!/usr/bin/env python3
"""Run release security gate v2: secret scan + SBOM + signature/redaction tests."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import shlex
import subprocess
import sys
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--secret-scan-cmd",
        default="python3 scripts/release_secret_scan.py --repo-root . --report .sync-reports/release-secret-scan.json",
        help="Secret scan command",
    )
    parser.add_argument(
        "--signature-tests-cmd",
        default="cargo test -p hermes-cli provenance_verify_detects_ -- --nocapture",
        help="Signature verification regression tests",
    )
    parser.add_argument(
        "--redaction-tests-cmd",
        default="cargo test -p hermes-gateway test_redact_pii -- --nocapture",
        help="Redaction regression tests",
    )
    parser.add_argument(
        "--sbom-output",
        default=".sync-reports/release-sbom-metadata.json",
        help="SBOM output path",
    )
    parser.add_argument(
        "--report",
        default="",
        help="Gate report output path (defaults to .sync-reports/security-release-gate-v2-*.json)",
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


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"security-release-gate-v2-{stamp}.json"


def generate_sbom(repo_root: pathlib.Path, output: pathlib.Path) -> dict[str, Any]:
    output.parent.mkdir(parents=True, exist_ok=True)
    proc = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
    )
    result: dict[str, Any] = {
        "command": "cargo metadata --format-version 1 --locked",
        "exit_code": proc.returncode,
        "ok": proc.returncode == 0,
        "output": str(output),
        "stdout_tail": proc.stdout[-6000:],
        "stderr_tail": proc.stderr[-6000:],
    }
    if proc.returncode != 0:
        return result

    try:
        metadata = json.loads(proc.stdout)
    except Exception as exc:
        result["ok"] = False
        result["error"] = f"failed to parse cargo metadata json: {exc}"
        return result

    packages = metadata.get("packages") or []
    workspace_members = metadata.get("workspace_members") or []
    nodes = (metadata.get("resolve") or {}).get("nodes") or []

    summary = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "format": "cargo-metadata-v1",
        "package_count": len(packages),
        "workspace_member_count": len(workspace_members),
        "resolve_node_count": len(nodes),
        "packages": [
            {
                "name": pkg.get("name"),
                "version": pkg.get("version"),
                "id": pkg.get("id"),
                "license": pkg.get("license"),
                "repository": pkg.get("repository"),
            }
            for pkg in packages
        ],
    }
    output.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    result["package_count"] = len(packages)
    result["workspace_member_count"] = len(workspace_members)
    return result


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")

    sbom_output = pathlib.Path(args.sbom_output)
    if not sbom_output.is_absolute():
        sbom_output = repo_root / sbom_output

    sections = {
        "secret_scan": run_shell(args.secret_scan_cmd, repo_root),
        "sbom": generate_sbom(repo_root, sbom_output),
        "signature_tests": run_shell(args.signature_tests_cmd, repo_root),
        "redaction_tests": run_shell(args.redaction_tests_cmd, repo_root),
    }

    ok = all(bool(section.get("ok")) for section in sections.values())
    report_path = pathlib.Path(args.report).expanduser().resolve() if args.report else default_report_path(repo_root)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "ok": ok,
        "summary": {
            "passed_sections": sum(1 for s in sections.values() if s.get("ok")),
            "failed_sections": sum(1 for s in sections.values() if not s.get("ok")),
            "total_sections": len(sections),
        },
        "sections": sections,
        "report_path": str(report_path),
    }
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        status = "PASSED" if ok else "FAILED"
        print(
            f"[security-release-gate-v2] {status} "
            f"(passed={report['summary']['passed_sections']}/{report['summary']['total_sections']})"
        )
        print(f"[security-release-gate-v2] SBOM: {sbom_output}")
        print(f"[security-release-gate-v2] Report: {report_path}")

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
