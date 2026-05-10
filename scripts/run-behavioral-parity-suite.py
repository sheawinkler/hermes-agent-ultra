#!/usr/bin/env python3
"""Behavior-level parity contract for Hermes Ultra command + TUI surfaces."""

from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path


REQUIRED_COMMAND_TOKENS = [
    "/plan caps",
    "/autocompact governance",
    "/model failover",
    "/ops cockpit",
    "/skills quality",
    "/raw trace focus",
    "/raw trace graph",
]

REQUIRED_TUI_TOKENS = [
    "ActivityLaneMode::Cockpit",
    "Ctrl+O cockpit",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument(
        "--report",
        default="",
        help="Optional report path (default: .sync-reports/behavioral-parity-*.json)",
    )
    return parser.parse_args()


def default_report_path(repo_root: Path) -> Path:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    return repo_root / ".sync-reports" / f"behavioral-parity-{stamp}.json"


def missing_tokens(raw: str, tokens: list[str]) -> list[str]:
    return [token for token in tokens if token not in raw]


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).resolve()
    commands_rs = repo_root / "crates/hermes-cli/src/commands.rs"
    tui_rs = repo_root / "crates/hermes-cli/src/tui.rs"

    commands_raw = commands_rs.read_text(encoding="utf-8")
    tui_raw = tui_rs.read_text(encoding="utf-8")

    missing_command_tokens = missing_tokens(commands_raw, REQUIRED_COMMAND_TOKENS)
    missing_tui_tokens = missing_tokens(tui_raw, REQUIRED_TUI_TOKENS)
    ok = not missing_command_tokens and not missing_tui_tokens

    payload = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "required_command_tokens": REQUIRED_COMMAND_TOKENS,
        "required_tui_tokens": REQUIRED_TUI_TOKENS,
        "missing_command_tokens": missing_command_tokens,
        "missing_tui_tokens": missing_tui_tokens,
        "ok": ok,
    }

    report_path = Path(args.report).resolve() if args.report else default_report_path(repo_root)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    status = "PASS" if ok else "FAIL"
    print(f"[behavioral-parity] {status}")
    print(f"[behavioral-parity] Report: {report_path}")
    if missing_command_tokens:
        print("[behavioral-parity] Missing command tokens:")
        for token in missing_command_tokens:
            print(f"  - {token}")
    if missing_tui_tokens:
        print("[behavioral-parity] Missing TUI tokens:")
        for token in missing_tui_tokens:
            print(f"  - {token}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
