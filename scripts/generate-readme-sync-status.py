#!/usr/bin/env python3
"""Render/update README upstream sync status block from .sync-reports artifacts."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

BEGIN_MARKER = "<!-- BEGIN:ULTRA_SYNC_STATUS -->"
END_MARKER = "<!-- END:ULTRA_SYNC_STATUS -->"
UPSTREAM_SECTION = "## Upstream Sync and Parity Upkeep"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".", help="Repository root")
    parser.add_argument("--readme", default="README.md", help="README path relative to repo root")
    parser.add_argument(
        "--report-dir",
        default=".sync-reports",
        help="Sync report dir relative to repo root",
    )
    parser.add_argument(
        "--queue-json",
        default="docs/parity/upstream-missing-queue.json",
        help="Queue summary JSON path relative to repo root",
    )
    parser.add_argument(
        "--proof-json",
        default="docs/parity/global-parity-proof.json",
        help="Global parity proof JSON path relative to repo root",
    )
    parser.add_argument(
        "--workstream-json",
        default="docs/parity/workstream-status.json",
        help="Workstream status JSON path relative to repo root",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Do not write files; exit non-zero if README block would change",
    )
    return parser.parse_args()


def load_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    return data if isinstance(data, dict) else {}


def latest_upstream_sync_report(report_dir: Path) -> Path | None:
    reports = sorted(report_dir.glob("upstream-sync-*.txt"))
    return reports[-1] if reports else None


def parse_sync_report(path: Path) -> tuple[dict[str, str], int]:
    meta: dict[str, str] = {}
    pending_count = 0
    text = path.read_text(encoding="utf-8", errors="replace")
    in_pending = False
    in_block = False

    for line in text.splitlines():
        stripped = line.strip()
        if not in_pending:
            if stripped.startswith("## Pending Upstream Commits"):
                in_pending = True
                continue
            if ":" in line:
                key, value = line.split(":", 1)
                key = key.strip()
                if key and all(ch.isalnum() or ch in {"_", "-"} for ch in key):
                    meta[key] = value.strip()
            continue

        if stripped == "```":
            if not in_block:
                in_block = True
            else:
                break
            continue
        if in_block and stripped:
            pending_count += 1

    return meta, pending_count


def gate_status(value: Any) -> str:
    if value is True:
        return "pass"
    if value is False:
        return "fail"
    return "unknown"


def build_block(
    generated_at: str,
    report_rel: str,
    report_name: str,
    report_meta: dict[str, str],
    report_pending_count: int,
    queue: dict[str, Any],
    proof: dict[str, Any],
    workstream: dict[str, Any],
) -> str:
    queue_summary = queue.get("summary", {}) if isinstance(queue.get("summary"), dict) else {}
    by_disp = queue_summary.get("by_disposition", {}) if isinstance(queue_summary.get("by_disposition"), dict) else {}
    pending = int(by_disp.get("pending", 0) or 0)
    ported = int(by_disp.get("ported", 0) or 0)
    superseded = int(by_disp.get("superseded", 0) or 0)

    release_gate = gate_status((proof.get("release_gate") or {}).get("pass") if isinstance(proof.get("release_gate"), dict) else None)
    ci_gate = gate_status((proof.get("ci_gate") or {}).get("pass") if isinstance(proof.get("ci_gate"), dict) else None)

    upstream_ref = str(workstream.get("upstream_ref") or "unknown")
    upstream_sha = str(workstream.get("upstream_sha") or "unknown")
    workstream_generated = str(workstream.get("generated_at_utc") or "unknown")

    timestamp_utc = report_meta.get("timestamp_utc", "unknown")
    origin_sha = report_meta.get("origin_sha", "unknown")
    upstream_sync_sha = report_meta.get("upstream_sha", "unknown")

    lines = [
        BEGIN_MARKER,
        "### Live Upstream Sync Status (auto-generated)",
        "",
        f"- Generated at: `{generated_at}`",
        f"- Source report: [`{report_name}`](./{report_rel})",
        f"- Sync timestamp (`timestamp_utc`): `{timestamp_utc}`",
        f"- `origin/main` at sync: `{origin_sha}`",
        f"- `upstream/main` at sync: `{upstream_sync_sha}`",
        f"- Pending commits captured in report: `{report_pending_count}`",
        (
            "- Queue summary (`docs/parity/upstream-missing-queue.json`): "
            f"pending `{pending}`, ported `{ported}`, superseded `{superseded}`"
        ),
        (
            "- Parity gates (`docs/parity/global-parity-proof.json`): "
            f"release `{release_gate}`, ci `{ci_gate}`"
        ),
        (
            "- Workstream snapshot (`docs/parity/workstream-status.json`): "
            f"`{upstream_ref}` @ `{upstream_sha}` (generated `{workstream_generated}`)"
        ),
        END_MARKER,
    ]
    return "\n".join(lines)


def replace_or_insert_block(readme: str, block: str) -> str:
    if BEGIN_MARKER in readme and END_MARKER in readme:
        start = readme.index(BEGIN_MARKER)
        end = readme.index(END_MARKER) + len(END_MARKER)
        return readme[:start] + block + readme[end:]

    if UPSTREAM_SECTION in readme:
        section_idx = readme.index(UPSTREAM_SECTION)
        section_end = readme.find("\n", section_idx)
        if section_end != -1:
            section_end += 1
            return readme[:section_end] + "\n" + block + "\n\n" + readme[section_end:]

    return readme.rstrip() + "\n\n" + block + "\n"


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root).expanduser().resolve()
    readme_path = (repo_root / args.readme).resolve()
    report_dir = (repo_root / args.report_dir).resolve()
    queue_path = (repo_root / args.queue_json).resolve()
    proof_path = (repo_root / args.proof_json).resolve()
    workstream_path = (repo_root / args.workstream_json).resolve()

    if not readme_path.exists():
        raise SystemExit(f"README not found: {readme_path}")
    if not report_dir.exists():
        raise SystemExit(f"report directory not found: {report_dir}")

    report = latest_upstream_sync_report(report_dir)
    if report is None:
        raise SystemExit(f"no upstream-sync reports found in {report_dir}")
    report_meta, report_pending_count = parse_sync_report(report)

    queue = load_json(queue_path)
    proof = load_json(proof_path)
    workstream = load_json(workstream_path)

    generated_at = report_meta.get("timestamp_utc", "") or report.stem.removeprefix("upstream-sync-")
    report_rel = str(report.relative_to(repo_root))
    block = build_block(
        generated_at=generated_at,
        report_rel=report_rel,
        report_name=report.name,
        report_meta=report_meta,
        report_pending_count=report_pending_count,
        queue=queue,
        proof=proof,
        workstream=workstream,
    )

    original = readme_path.read_text(encoding="utf-8")
    updated = replace_or_insert_block(original, block)

    if args.check:
        if original != updated:
            print("README sync status block is stale.")
            return 1
        print("README sync status block is up-to-date.")
        return 0

    if original != updated:
        readme_path.write_text(updated, encoding="utf-8")
        print(f"Updated README sync status block in {readme_path}")
    else:
        print(f"README sync status block already up-to-date in {readme_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
