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


def read_rust_surface(path: Path) -> str:
    parts: list[str] = []
    if path.is_file():
        parts.append(path.read_text(encoding="utf-8"))
    if path.is_dir():
        parts.extend(
            p.read_text(encoding="utf-8")
            for p in sorted(path.rglob("*.rs"))
            if p.is_file()
        )
    elif path.suffix == ".rs":
        split_dir = path.with_suffix("")
        if split_dir.is_dir():
            parts.extend(
                p.read_text(encoding="utf-8")
                for p in sorted(split_dir.rglob("*.rs"))
                if p.is_file()
            )
    if not parts:
        return path.read_text(encoding="utf-8")
    return "\n".join(parts)


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).resolve()
    commands_rs = repo_root / "crates/hermes-cli/src/commands"
    tui_rs = repo_root / "crates/hermes-cli/src/tui.rs"

    commands_raw = read_rust_surface(commands_rs)
    tui_raw = read_rust_surface(tui_rs)

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
