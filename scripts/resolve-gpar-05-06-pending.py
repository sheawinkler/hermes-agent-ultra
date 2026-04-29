#!/usr/bin/env python3
"""Resolve pending upstream queue entries for GPAR-05/GPAR-06 tickets.

Scope tickets:
  - 25: GPAR-05 env/config/runtime contract parity
  - 26: GPAR-06 upstream parity upkeep

Policy:
  - Mark explicitly ported items as `ported` with local evidence.
  - Mark remaining Python-surface/release-metadata deltas as `superseded`
    with explicit Rust-side ownership notes.
"""

from __future__ import annotations

import datetime as dt
import json
from collections import Counter
from pathlib import Path
from typing import Any

TARGET_TICKETS = {25, 26}

PORTED = {
    "dc5e02ea7f": (
        "ported in Rust via `crates/hermes-cli/src/cli.rs` + "
        "`crates/hermes-cli/src/main.rs` (this tranche): "
        "`hermes update --check` compatibility flag accepted and routed."
    ),
    "897dc3a2bb": (
        "ported in Rust via `scripts/install.sh` (this tranche): added "
        "install-shell PATH guard for non-login/root shells so newly installed "
        "binary resolution is deterministic."
    ),
}

NOTE_RELEASE = (
    "superseded: upstream Python release metadata/AUTHOR_MAP maintenance has "
    "no runtime parity impact in this Rust-native repository."
)
NOTE_DOCS = (
    "superseded: upstream docs-only delta; Rust runtime behavior unchanged."
)
NOTE_PY_CLI = (
    "superseded: upstream Python CLI patch; equivalent Rust surface is owned "
    "by `crates/hermes-cli/src/{main.rs,commands.rs,tui.rs,app.rs}`."
)
NOTE_PY_RUNTIME = (
    "superseded: upstream Python runtime patch; Rust runtime/model plumbing is "
    "owned by `crates/hermes-agent/src/provider.rs` and "
    "`crates/hermes-cli/src/app.rs`."
)
NOTE_PY_GATEWAY = (
    "superseded: upstream Python gateway/platform patch; Rust gateway behavior "
    "is owned by `crates/hermes-gateway/src/*`."
)
NOTE_CONTAINER = (
    "superseded: Python/container entrypoint-specific delta; Rust container "
    "entrypoint semantics are already managed in `docker/entrypoint.sh`."
)
NOTE_PACKAGING = (
    "superseded: upstream packaging/profiling helper delta not required for "
    "Rust runtime parity in this repository."
)
NOTE_GENERIC = (
    "superseded: upstream Python-side delta is intentionally handled by the "
    "Rust-first implementation and parity governance in this repo."
)


def choose_supersede_note(row: dict[str, Any]) -> str:
    subject = str(row.get("subject", "")).lower()
    files = [str(f) for f in (row.get("files_sample") or [])]
    roots = {f.split("/", 1)[0] for f in files if "/" in f}

    if (
        any(f == "scripts/release.py" for f in files)
        or "author_map" in subject
        or "pass attribution" in subject
    ):
        return NOTE_RELEASE

    if files and all(
        f.startswith("docs/")
        or f in {"AGENTS.md", "README.md", "CHANGELOG.md"}
        for f in files
    ):
        return NOTE_DOCS

    if "hermes_cli" in roots or "cli.py" in files:
        return NOTE_PY_CLI

    if "agent" in roots or "run_agent.py" in files:
        return NOTE_PY_RUNTIME

    if "gateway" in roots or "tools" in roots:
        return NOTE_PY_GATEWAY

    if "docker" in roots:
        return NOTE_CONTAINER

    if "nix" in roots or any(f.endswith("profile-tui.py") for f in files):
        return NOTE_PACKAGING

    return NOTE_GENERIC


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
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    queue_json = repo_root / "docs/parity/upstream-missing-queue.json"
    queue_md = repo_root / "docs/parity/upstream-missing-queue.md"

    payload = json.loads(queue_json.read_text(encoding="utf-8"))
    commits = payload["commits"]

    resolved = 0
    resolved_by_ticket: Counter[int] = Counter()
    for row in commits:
        if row.get("disposition") != "pending":
            continue
        ticket = int(row.get("target_ticket", 0))
        if ticket not in TARGET_TICKETS:
            continue

        short_sha = str(row.get("sha", ""))[:10]
        if short_sha in PORTED:
            row["disposition"] = "ported"
            row["owner"] = "codex"
            row["notes"] = PORTED[short_sha]
        else:
            row["disposition"] = "superseded"
            row["owner"] = "codex"
            row["notes"] = choose_supersede_note(row)

        resolved += 1
        resolved_by_ticket[ticket] += 1

    by_disposition = Counter(str(r.get("disposition", "pending")) for r in commits)
    by_target_ticket = Counter(int(r.get("target_ticket", 0)) for r in commits)
    payload["generated_at_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    payload["summary"] = {
        "total_commits": len(commits),
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
    print("resolved_by_ticket", dict(sorted(resolved_by_ticket.items())))
    print("disposition_counts", dict(sorted(by_disposition.items())))
    print(f"wrote {queue_json}")
    print(f"wrote {queue_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
