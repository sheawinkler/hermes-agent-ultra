#!/usr/bin/env python3
"""Generate a deterministic file-level diff map for snapshot-style upstream refs.

When upstream history collapses into a large snapshot commit, commit-level parity
queues become low-signal. This script groups `git diff --name-status` output by
path prefix and emits auditable JSON/Markdown artifacts for tranche planning.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

RUST_PRIMARY_PREFIXES = {"crates"}
RUST_PRIMARY_FILES = {"Cargo.toml", "Cargo.lock"}

UPSTREAM_RUNTIME_PREFIXES = {
    "agent",
    "gateway",
    "hermes_cli",
    "tools",
    "plugins",
    "web",
    "website",
    "ui-tui",
    "skills",
    "optional-skills",
    "tests",
    "tui_gateway",
    "environments",
    "acp_adapter",
    "cron",
}

INFRA_PREFIXES = {
    ".github",
    "scripts",
    "packaging",
    "nix",
    "docker",
}
INFRA_FILES = {
    ".gitignore",
    "README.md",
    "AGENTS.md",
    "Dockerfile",
    "flake.nix",
    "LICENSE",
}

CLASSIFICATION_RATIONALE = {
    "intentional_divergence_rust_primary": (
        "Rust workspace remains first-class in this fork; upstream snapshot is Python-first."
    ),
    "needs_rust_implementation_review": (
        "Upstream runtime/product surface changed; evaluate and port behavior in Rust where relevant."
    ),
    "selective_adopt_review": (
        "Installer/packaging/docs/infra deltas may be partially applicable and should be reviewed."
    ),
    "manual_review_required": (
        "Path does not match a known tranche bucket; requires explicit manual classification."
    ),
}


def run_git(repo_root: Path, args: list[str], check: bool = True) -> str:
    proc = subprocess.run(
        ["git", *args], cwd=repo_root, text=True, capture_output=True
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {proc.stderr.strip()}")
    return proc.stdout


def normalize_prefix(path: str) -> str:
    if "/" in path:
        return path.split("/", 1)[0]
    return path


def classify_prefix(prefix: str) -> str:
    if prefix in RUST_PRIMARY_PREFIXES or prefix in RUST_PRIMARY_FILES:
        return "intentional_divergence_rust_primary"
    if prefix in UPSTREAM_RUNTIME_PREFIXES:
        return "needs_rust_implementation_review"
    if prefix in INFRA_PREFIXES or prefix in INFRA_FILES:
        return "selective_adopt_review"
    return "manual_review_required"


def parse_diff_rows(raw: str) -> list[dict[str, str]]:
    rows: list[dict[str, str]] = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        status = parts[0].strip()
        if status.startswith("R") or status.startswith("C"):
            # rename/copy: STATUS old new
            if len(parts) < 3:
                continue
            old_path = parts[1].strip()
            new_path = parts[2].strip()
            rows.append(
                {
                    "status": status[0],
                    "path": new_path,
                    "old_path": old_path,
                }
            )
            continue
        rows.append({"status": status[0], "path": parts[1].strip()})
    return rows


def summarize_tranches(groups: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    tranche_prefixes = {
        "runtime_surface": {"agent", "gateway", "hermes_cli", "tools", "plugins"},
        "ux_surface": {"ui-tui", "web", "website", "skills", "optional-skills"},
        "validation_surface": {"tests", "environments"},
        "infra_surface": {"scripts", "packaging", ".github", "nix", "docker"},
        "rust_divergence": {"crates", "Cargo.toml", "Cargo.lock"},
    }
    result: dict[str, dict[str, Any]] = {}
    for tranche, prefixes in tranche_prefixes.items():
        selected = [g for g in groups if g["prefix"] in prefixes]
        status_counts: Counter[str] = Counter()
        total = 0
        for g in selected:
            total += g["total"]
            status_counts.update(g["status_counts"])
        result[tranche] = {
            "total": total,
            "status_counts": dict(sorted(status_counts.items())),
            "prefixes": sorted(prefixes),
        }
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument("--local-ref", default="main")
    parser.add_argument("--upstream-ref", default="upstream/main")
    parser.add_argument(
        "--out-json",
        default="docs/parity/upstream-snapshot-diff-map.json",
        type=Path,
        help="Output JSON path (relative to repo root)",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/upstream-snapshot-diff-map.md",
        type=Path,
        help="Output Markdown path (relative to repo root)",
    )
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    diff_raw = run_git(
        repo_root,
        [
            "diff",
            "--name-status",
            "--find-renames",
            args.local_ref,
            args.upstream_ref,
        ],
    )
    rows = parse_diff_rows(diff_raw)

    status_counts: Counter[str] = Counter()
    by_prefix: dict[str, list[dict[str, str]]] = defaultdict(list)
    for row in rows:
        status_counts[row["status"]] += 1
        prefix = normalize_prefix(row["path"])
        by_prefix[prefix].append(row)

    groups: list[dict[str, Any]] = []
    for prefix, entries in by_prefix.items():
        group_status: Counter[str] = Counter(e["status"] for e in entries)
        classification = classify_prefix(prefix)
        groups.append(
            {
                "prefix": prefix,
                "total": len(entries),
                "status_counts": dict(sorted(group_status.items())),
                "classification": classification,
                "rationale": CLASSIFICATION_RATIONALE[classification],
                "sample_paths": sorted({e["path"] for e in entries})[:15],
            }
        )
    groups.sort(key=lambda g: (-g["total"], g["prefix"]))

    payload: dict[str, Any] = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "refs": {
            "local_ref": args.local_ref,
            "upstream_ref": args.upstream_ref,
        },
        "summary": {
            "total_entries": len(rows),
            "status_counts": dict(sorted(status_counts.items())),
            "group_count": len(groups),
        },
        "groups": groups,
        "tranche_summary": summarize_tranches(groups),
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md: list[str] = []
    md.append("# Upstream Snapshot Diff Map")
    md.append("")
    md.append(f"Generated: `{payload['generated_at_utc']}`")
    md.append("")
    md.append(
        f"- Compared refs: `{args.local_ref}` vs `{args.upstream_ref}`"
    )
    md.append(f"- Total diff entries: `{payload['summary']['total_entries']}`")
    md.append(
        "- Status counts: "
        + ", ".join(
            f"`{k}={v}`" for k, v in payload["summary"]["status_counts"].items()
        )
    )
    md.append("")
    md.append("## Prefix Groups")
    md.append("")
    md.append(
        "| Prefix | Total | A | M | D | Classification |"
    )
    md.append("| --- | ---: | ---: | ---: | ---: | --- |")
    for g in groups:
        md.append(
            f"| `{g['prefix']}` | {g['total']} | "
            f"{g['status_counts'].get('A', 0)} | "
            f"{g['status_counts'].get('M', 0)} | "
            f"{g['status_counts'].get('D', 0)} | "
            f"`{g['classification']}` |"
        )
    md.append("")
    md.append("## Tranche Summary")
    md.append("")
    md.append("| Tranche | Total | Status Counts |")
    md.append("| --- | ---: | --- |")
    for name, info in sorted(payload["tranche_summary"].items()):
        counts = ", ".join(f"{k}={v}" for k, v in info["status_counts"].items()) or "none"
        md.append(f"| `{name}` | {info['total']} | `{counts}` |")
    md.append("")
    md.append("## Notes")
    md.append("")
    md.append(
        "- `intentional_divergence_rust_primary` marks Rust-first surfaces kept on purpose in this fork."
    )
    md.append(
        "- `needs_rust_implementation_review` marks upstream product/runtime paths to review for behavior-level parity."
    )
    md.append(
        "- `selective_adopt_review` marks installer/docs/infra paths that may need partial adoption."
    )
    md.append(
        "- `manual_review_required` marks uncategorized paths requiring explicit triage."
    )
    md.append("")

    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")
    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
