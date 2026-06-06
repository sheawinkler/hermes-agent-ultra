#!/usr/bin/env python3
"""Resolve pending upstream queue entries for GPAR-05 environments/parsers."""

from __future__ import annotations

import datetime as dt
import json
from collections import Counter
from pathlib import Path
from typing import Any

TARGET_TICKET = 24

NOTES_BY_SHA_PREFIX = {
    "5af672c753": (
        "superseded: upstream removed Python Atropos/tinker integration; this "
        "Rust-first tree has no `tinker-atropos` source checkout, and training "
        "environment ownership is tracked in `crates/hermes-environments/src/training/mod.rs`."
    ),
    "62573f44cf": (
        "ported in Rust via `crates/hermes-environments/src/file_sync.rs` "
        "(this tranche): `sync_from_remote` now writes through a same-directory "
        "temporary file, fsyncs it, and rename-replaces the destination. "
        "Other upstream yaml/flock Python surfaces are absent from the Rust runtime."
    ),
}

DISPOSITION_BY_SHA_PREFIX = {
    "5af672c753": "superseded",
    "62573f44cf": "ported",
}


def render_queue_md(payload: dict[str, Any]) -> str:
    rows = payload["commits"]
    by_ticket: Counter[int] = Counter(int(r["target_ticket"]) for r in rows)
    by_disposition: Counter[str] = Counter(str(r["disposition"]) for r in rows)

    lines: list[str] = []
    lines.append("# Upstream Missing Patch Queue")
    lines.append("")
    lines.append(f"Generated: `{payload['generated_at_utc']}`")
    lines.append("")
    refs = payload["refs"]
    lines.append(
        f"- Range: `{refs['range']}`; total commits tracked: `{payload['summary']['total_commits']}`."
    )
    lines.append("")
    lines.append("| Ticket | Label | Commit Count |")
    lines.append("| ---: | --- | ---: |")

    ticket_labels = {}
    for row in rows:
        ticket_labels[int(row["target_ticket"])] = str(row.get("target_ticket_name", "unknown"))

    for ticket, count in sorted(by_ticket.items()):
        lines.append(f"| #{ticket} | {ticket_labels.get(ticket, 'unknown')} | {count} |")

    lines.append("")
    lines.append("| Disposition | Commit Count |")
    lines.append("| --- | ---: |")
    for disposition, count in sorted(by_disposition.items()):
        lines.append(f"| {disposition} | {count} |")

    lines.append("")
    lines.append("## First 100 Pending Commits")
    lines.append("")
    lines.append("| SHA | Ticket | Subject |")
    lines.append("| --- | ---: | --- |")
    pending_rows = [r for r in rows if r["disposition"] == "pending"][:100]
    if not pending_rows:
        lines.append("| _(none)_ | - | - |")
    else:
        for row in pending_rows:
            subject = str(row.get("subject", "")).replace("|", "\\|")
            lines.append(f"| `{row['sha'][:12]}` | #{row['target_ticket']} | {subject} |")
    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    queue_json = repo_root / "docs/parity/upstream-missing-queue.json"
    queue_md = repo_root / "docs/parity/upstream-missing-queue.md"

    payload = json.loads(queue_json.read_text(encoding="utf-8"))
    resolved = 0
    resolved_by_sha: list[str] = []
    for row in payload["commits"]:
        if row.get("disposition") != "pending":
            continue
        if int(row.get("target_ticket", 0)) != TARGET_TICKET:
            continue

        short_sha = str(row.get("sha", ""))[:10]
        note = NOTES_BY_SHA_PREFIX.get(short_sha)
        disposition = DISPOSITION_BY_SHA_PREFIX.get(short_sha)
        if not note or not disposition:
            continue

        row["disposition"] = disposition
        row["owner"] = "codex"
        row["notes"] = note
        resolved += 1
        resolved_by_sha.append(short_sha)

    by_disposition = Counter(str(r.get("disposition", "pending")) for r in payload["commits"])
    by_target_ticket = Counter(int(r.get("target_ticket", 0)) for r in payload["commits"])
    payload["generated_at_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    payload["summary"] = {
        "total_commits": len(payload["commits"]),
        "by_target_ticket": {
            str(k): v for k, v in sorted(by_target_ticket.items()) if k != 0
        },
        "by_disposition": dict(sorted(by_disposition.items())),
    }

    queue_json.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    queue_md.write_text(render_queue_md(payload) + "\n", encoding="utf-8")

    print("resolved_rows", resolved)
    print("resolved_by_sha", resolved_by_sha)
    print("disposition_counts", dict(sorted(by_disposition.items())))
    print(f"wrote {queue_json}")
    print(f"wrote {queue_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
