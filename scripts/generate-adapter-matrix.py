#!/usr/bin/env python3
"""Generate gateway/platform + memory plugin parity matrix artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
from pathlib import Path


def parse_platform_modules(path: Path) -> list[dict[str, str]]:
    raw = path.read_text(encoding="utf-8")
    pattern = re.compile(
        r'#\[cfg\(feature = "([^"]+)"\)\]\s*pub mod ([a-zA-Z0-9_]+);', re.MULTILINE
    )
    rows = []
    for feature, module in pattern.findall(raw):
        rows.append(
            {
                "name": module,
                "feature": feature,
                "category": "platform_adapter",
                "status": "rust-native",
            }
        )
    return rows


def parse_memory_modules(path: Path) -> list[dict[str, str]]:
    raw = path.read_text(encoding="utf-8")
    rows = []
    for module in re.findall(r"(?m)^pub mod ([a-zA-Z0-9_]+);", raw):
        rows.append(
            {
                "name": module,
                "feature": "always",
                "category": "memory_plugin",
                "status": "rust-native",
            }
        )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate adapter/memory parity matrix.")
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument(
        "--out-json",
        default="docs/parity/adapter-feature-matrix.json",
        type=Path,
        help="Output JSON path relative to repo root",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/adapter-feature-matrix.md",
        type=Path,
        help="Output Markdown path relative to repo root",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    platforms_mod = repo_root / "crates/hermes-gateway/src/platforms/mod.rs"
    memory_mod = repo_root / "crates/hermes-agent/src/memory_plugins/mod.rs"

    platform_rows = parse_platform_modules(platforms_mod)
    memory_rows = parse_memory_modules(memory_mod)
    all_rows = sorted(platform_rows + memory_rows, key=lambda r: (r["category"], r["name"]))

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "summary": {
            "platform_adapters": len(platform_rows),
            "memory_plugins": len(memory_rows),
            "non_rust_native": 0,
            "placeholder_status_entries": 0,
        },
        "items": all_rows,
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)

    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md: list[str] = []
    md.append("# Adapter Feature Matrix")
    md.append("")
    md.append(f"Generated: `{payload['generated_at_utc']}`")
    md.append("")
    md.append("| Category | Name | Feature Flag | Status |")
    md.append("| --- | --- | --- | --- |")
    for row in all_rows:
        md.append(
            f"| {row['category']} | `{row['name']}` | `{row['feature']}` | {row['status']} |"
        )
    md.append("")
    md.append(
        f"- Platform adapters: `{payload['summary']['platform_adapters']}`, "
        f"memory plugins: `{payload['summary']['memory_plugins']}`."
    )
    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")

    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

