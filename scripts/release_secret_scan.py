#!/usr/bin/env python3
"""Release-time secret scanning gate for tracked repository files."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys
from dataclasses import dataclass


PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("openai_key", re.compile(r"\bsk-[A-Za-z0-9]{20,}\b")),
    ("anthropic_key", re.compile(r"\bsk-ant-[A-Za-z0-9\-_]{20,}\b")),
    ("github_pat", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b")),
    ("telegram_bot_token", re.compile(r"\b\d{8,10}:[A-Za-z0-9_-]{30,}\b")),
    ("aws_access_key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("slack_token", re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{16,}\b")),
    ("private_key_block", re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----")),
]

SKIP_PREFIXES = (
    ".git/",
    "target/",
    "node_modules/",
    ".sync-reports/",
)

SKIP_LINE_HINTS = (
    "example",
    "redacted",
    "dummy",
    "placeholder",
    "test-key",
    "fake",
    "xxxx",
    "changeme",
    "detect_secrets(",
    "sk-abc123def456ghi789jkl012mno345",
    "akia1234567890abcdef",
    "abcdefghijklmnopqrstuvwxyzabcd1234",
    "miiEvg".lower(),
    "1234567890:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
)


@dataclass
class Finding:
    file: str
    line: int
    pattern: str
    snippet: str


def repo_files(repo_root: pathlib.Path) -> list[pathlib.Path]:
    proc = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=repo_root,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise SystemExit("git ls-files failed; run from a git repository root")
    out = proc.stdout.decode("utf-8", errors="ignore")
    paths = [p for p in out.split("\x00") if p]
    return [repo_root / p for p in paths]


def should_skip_file(rel_path: str) -> bool:
    normalized = rel_path.replace("\\", "/")
    if any(normalized.startswith(prefix) for prefix in SKIP_PREFIXES):
        return True
    if normalized.endswith((".png", ".jpg", ".jpeg", ".gif", ".pdf", ".ico", ".wasm")):
        return True
    return False


def should_skip_line(line: str) -> bool:
    lower = line.lower()
    return any(hint in lower for hint in SKIP_LINE_HINTS)


def is_text(path: pathlib.Path) -> bool:
    try:
        data = path.read_bytes()
    except Exception:
        return False
    return b"\x00" not in data


def scan(repo_root: pathlib.Path) -> list[Finding]:
    findings: list[Finding] = []
    for file_path in repo_files(repo_root):
        rel = file_path.relative_to(repo_root).as_posix()
        if should_skip_file(rel) or not file_path.exists() or not file_path.is_file():
            continue
        if not is_text(file_path):
            continue
        try:
            content = file_path.read_text(encoding="utf-8", errors="ignore")
        except Exception:
            continue
        for line_no, line in enumerate(content.splitlines(), start=1):
            if should_skip_line(line):
                continue
            for name, pattern in PATTERNS:
                if pattern.search(line):
                    snippet = line.strip()
                    if len(snippet) > 180:
                        snippet = snippet[:177] + "..."
                    findings.append(
                        Finding(file=rel, line=line_no, pattern=name, snippet=snippet)
                    )
    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--report",
        default="",
        help="Optional JSON report output path",
    )
    args = parser.parse_args()

    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    findings = scan(repo_root)

    report = {
        "repo_root": str(repo_root),
        "total_findings": len(findings),
        "findings": [finding.__dict__ for finding in findings],
    }
    if args.report:
        report_path = pathlib.Path(args.report)
        if not report_path.is_absolute():
            report_path = repo_root / report_path
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if findings:
        print("[release-secret-scan] FAILED: potential secrets detected")
        for finding in findings[:50]:
            print(
                f"- {finding.file}:{finding.line} [{finding.pattern}] {finding.snippet}",
                file=sys.stderr,
            )
        return 1

    print("[release-secret-scan] OK: no tracked secret patterns detected")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
