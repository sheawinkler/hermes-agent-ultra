#!/usr/bin/env python3
"""Golden parity harness for CLI command surface + key TUI flow contracts."""

from __future__ import annotations

import argparse
import datetime as dt
import importlib
import json
import pathlib
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Any


TITLE_PREFIX = "[Parity] Golden Harness Drift"

REQUIRED_TUI_TESTS = [
    "test_completion_popup_hidden_when_slash_deleted",
    "test_completion_popup_hidden_when_modal_or_processing_active",
    "test_transcript_hides_system_messages",
    "test_stream_handle",
    "test_is_ctrl_c_detection",
]

REQUIRED_COMMAND_CONTRACTS = {
    "/model": ["capability", "explain"],
    "/raw": ["trace", "deterministic"],
    "/policy": ["profile"],
}


@dataclass
class HarnessReport:
    generated_at: str
    repo_root: str
    upstream_root: str
    missing_commands: list[str]
    extra_commands: list[str]
    missing_tui_tests: list[str]
    command_contract_failures: list[str]
    allow_missing_commands: int
    ok: bool

    def to_dict(self) -> dict[str, Any]:
        return {
            "generated_at": self.generated_at,
            "repo_root": self.repo_root,
            "upstream_root": self.upstream_root,
            "missing_commands": self.missing_commands,
            "extra_commands": self.extra_commands,
            "missing_tui_tests": self.missing_tui_tests,
            "command_contract_failures": self.command_contract_failures,
            "allow_missing_commands": self.allow_missing_commands,
            "ok": self.ok,
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Hermes Ultra repository root")
    parser.add_argument(
        "--upstream-root",
        default="../hermes-agent",
        help="Path to upstream NousResearch hermes-agent checkout",
    )
    parser.add_argument(
        "--report",
        default="",
        help="Optional report output path (defaults to .sync-reports/golden-parity-harness-*.json)",
    )
    parser.add_argument(
        "--allow-missing",
        type=int,
        default=-1,
        help="Maximum allowed missing upstream commands before failing",
    )
    parser.add_argument(
        "--baseline",
        default="docs/parity/golden-harness-baseline.json",
        help="Optional baseline JSON with max_missing_commands field",
    )
    parser.add_argument(
        "--auto-issue",
        action="store_true",
        help="Auto-open/update a parity drift issue when failures are detected",
    )
    return parser.parse_args()


def parse_local_slash_commands(commands_rs: pathlib.Path) -> set[str]:
    raw = commands_rs.read_text(encoding="utf-8")
    matches = re.findall(r'\(\s*"(/[^"]+)"\s*,', raw)
    return {m.strip() for m in matches if m.strip().startswith("/")}


def parse_local_command_descriptions(commands_rs: pathlib.Path) -> dict[str, str]:
    raw = commands_rs.read_text(encoding="utf-8")
    pairs = re.findall(r'\(\s*"(/[^"]+)"\s*,\s*"([^"]*)"\s*,?\s*\)', raw)
    return {cmd.strip(): desc.strip() for cmd, desc in pairs if cmd.strip().startswith("/")}


def parse_upstream_slash_commands(upstream_root: pathlib.Path) -> set[str]:
    sys.path.insert(0, str(upstream_root))
    try:
        module = importlib.import_module("hermes_cli.commands")
    except Exception as exc:
        raise RuntimeError(f"failed to import upstream hermes_cli.commands: {exc}") from exc
    commands = getattr(module, "COMMANDS", {})
    if not isinstance(commands, dict):
        raise RuntimeError("upstream COMMANDS is not a dict")
    return {str(key).strip() for key in commands.keys() if str(key).startswith("/")}


def parse_tui_tests(tui_rs: pathlib.Path) -> set[str]:
    raw = tui_rs.read_text(encoding="utf-8")
    return set(re.findall(r"fn\s+([a-zA-Z0-9_]+)\s*\(", raw))


def load_allow_missing(args: argparse.Namespace, repo_root: pathlib.Path) -> int:
    if args.allow_missing >= 0:
        return args.allow_missing
    baseline_path = pathlib.Path(args.baseline)
    if not baseline_path.is_absolute():
        baseline_path = repo_root / baseline_path
    try:
        payload = json.loads(baseline_path.read_text(encoding="utf-8"))
    except Exception:
        return 0
    value = payload.get("max_missing_commands")
    if isinstance(value, int) and value >= 0:
        return value
    return 0


def build_report(repo_root: pathlib.Path, upstream_root: pathlib.Path, allow_missing: int) -> HarnessReport:
    commands_rs = repo_root / "crates/hermes-cli/src/commands.rs"
    local = parse_local_slash_commands(commands_rs)
    local_desc = parse_local_command_descriptions(commands_rs)
    upstream = parse_upstream_slash_commands(upstream_root)
    missing = sorted(upstream - local)
    extra = sorted(local - upstream)
    contract_failures: list[str] = []
    for command, required_terms in REQUIRED_COMMAND_CONTRACTS.items():
        desc = local_desc.get(command, "")
        desc_lc = desc.lower()
        missing_terms = [term for term in required_terms if term.lower() not in desc_lc]
        if missing_terms:
            contract_failures.append(
                f"{command}: description missing required terms {missing_terms!r}"
            )

    tui_tests = parse_tui_tests(repo_root / "crates/hermes-cli/src/tui.rs")
    missing_tui_tests = [name for name in REQUIRED_TUI_TESTS if name not in tui_tests]
    ok = (
        len(missing) <= allow_missing
        and not missing_tui_tests
        and not contract_failures
    )
    return HarnessReport(
        generated_at=dt.datetime.now(dt.timezone.utc).isoformat(),
        repo_root=str(repo_root),
        upstream_root=str(upstream_root),
        missing_commands=missing,
        extra_commands=extra,
        missing_tui_tests=missing_tui_tests,
        command_contract_failures=contract_failures,
        allow_missing_commands=allow_missing,
        ok=ok,
    )


def default_report_path(repo_root: pathlib.Path) -> pathlib.Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"golden-parity-harness-{stamp}.json"


def gh_available(repo_root: pathlib.Path) -> bool:
    try:
        proc = subprocess.run(
            ["gh", "--version"],
            cwd=repo_root,
            capture_output=True,
            text=True,
            check=False,
        )
        return proc.returncode == 0
    except Exception:
        return False


def run_gh(repo_root: pathlib.Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["gh", *args],
        cwd=repo_root,
        capture_output=True,
        text=True,
        check=False,
    )


def maybe_open_issue(repo_root: pathlib.Path, report: HarnessReport) -> None:
    if report.ok or not gh_available(repo_root):
        return

    summary_lines = [
        "Golden parity harness detected drift.",
        "",
        f"- Missing upstream commands: `{len(report.missing_commands)}`",
        f"- Missing required TUI tests: `{len(report.missing_tui_tests)}`",
        f"- Command contract failures: `{len(report.command_contract_failures)}`",
        "",
    ]
    if report.missing_commands:
        summary_lines.append("Missing commands:")
        summary_lines.extend([f"- `{cmd}`" for cmd in report.missing_commands[:50]])
        summary_lines.append("")
    if report.missing_tui_tests:
        summary_lines.append("Missing TUI tests:")
        summary_lines.extend([f"- `{name}`" for name in report.missing_tui_tests])
        summary_lines.append("")
    if report.command_contract_failures:
        summary_lines.append("Command contract failures:")
        summary_lines.extend([f"- {line}" for line in report.command_contract_failures[:50]])
        summary_lines.append("")
    body = "\n".join(summary_lines).strip()

    list_proc = run_gh(
        repo_root,
        [
            "issue",
            "list",
            "--state",
            "open",
            "--search",
            f"{TITLE_PREFIX} in:title",
            "--json",
            "number,title",
            "--limit",
            "1",
        ],
    )
    if list_proc.returncode == 0:
        try:
            rows = json.loads(list_proc.stdout or "[]")
        except json.JSONDecodeError:
            rows = []
        if rows:
            issue_number = str(rows[0]["number"])
            run_gh(repo_root, ["issue", "comment", issue_number, "--body", body])
            return

    title = f"{TITLE_PREFIX} ({dt.datetime.now(dt.timezone.utc).strftime('%Y-%m-%d')})"
    create = run_gh(repo_root, ["issue", "create", "--title", title, "--body", body, "--label", "parity"])
    if create.returncode != 0:
        run_gh(repo_root, ["issue", "create", "--title", title, "--body", body])


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    upstream_root = pathlib.Path(args.upstream_root).expanduser()
    if not upstream_root.is_absolute():
        upstream_root = (repo_root / upstream_root).resolve()

    if not repo_root.exists():
        raise SystemExit(f"repo root does not exist: {repo_root}")
    if not upstream_root.exists():
        raise SystemExit(f"upstream root does not exist: {upstream_root}")

    allow_missing = load_allow_missing(args, repo_root)
    report = build_report(repo_root, upstream_root, allow_missing)
    report_path = (
        pathlib.Path(args.report).expanduser().resolve()
        if args.report
        else default_report_path(repo_root)
    )
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report.to_dict(), indent=2) + "\n", encoding="utf-8")

    print(
        "[golden-parity] "
        f"{'PASSED' if report.ok else 'FAILED'} "
        f"(missing={len(report.missing_commands)} extra={len(report.extra_commands)} "
        f"missing_tui_tests={len(report.missing_tui_tests)} "
        f"contract_failures={len(report.command_contract_failures)} "
        f"allow_missing={allow_missing})"
    )
    print(f"[golden-parity] Report: {report_path}")

    if args.auto_issue:
        maybe_open_issue(repo_root, report)

    return 0 if report.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
