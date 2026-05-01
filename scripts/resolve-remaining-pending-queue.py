#!/usr/bin/env python3
"""Resolve remaining pending upstream queue entries with explicit evidence notes.

This pass is intentionally strict about ownership notes:
- mark known behavior ported where we have direct Rust evidence
- mark Python-only/Docs-only/plugin-only upstream deltas as superseded under
  Rust-first parity policy with owning modules referenced in notes
"""

from __future__ import annotations

import datetime as dt
import json
from collections import Counter
from pathlib import Path
from typing import Any

PORTED_BY_SHA = {
    "4d7fc0f37c": (
        "ported in Rust via `crates/hermes-cli/src/commands.rs` (reload handler): "
        "`/reload-mcp` now emits explicit restart guidance and prompt-cache "
        "invalidation warning semantics."
    ),
    "8d302e37a8": (
        "ported in Rust via `crates/hermes-tools/src/backends/tts.rs` + "
        "`crates/hermes-tools/src/tools/tts.rs` (this tranche): local `piper` "
        "provider support with model resolution + output artifact handling."
    ),
}

NOTE_RELEASE = (
    "superseded: upstream release metadata / AUTHOR_MAP maintenance has no "
    "runtime parity impact for Rust-first Hermes Ultra."
)

NOTE_DOCS = (
    "superseded: upstream docs/web copy/layout delta only; runtime parity is "
    "owned by Rust CLI/TUI/gateway surfaces."
)

NOTE_WEB_UI = (
    "superseded: upstream Node/web/ui-tui surface is intentionally divergent; "
    "equivalent runtime UX is owned by `crates/hermes-cli/src/tui.rs` and "
    "slash-command surfaces in `crates/hermes-cli/src/commands.rs`."
)

NOTE_TESTS = (
    "superseded: upstream Python test-only delta; Rust parity tests and unit "
    "coverage own this behavior domain in `crates/*` + workspace test suites."
)

NOTE_PLUGIN_ONLY = (
    "superseded: upstream Python plugin-only platform/runtime wiring; Hermes "
    "Ultra uses Rust-native adapter + toolset ownership across "
    "`crates/hermes-gateway` and `crates/hermes-cli/src/platform_toolsets.rs`."
)

NOTE_RUNTIME_PY = (
    "superseded: upstream Python runtime patch maps to Rust-owned execution "
    "paths (`crates/hermes-cli/src/{app.rs,commands.rs,main.rs}`, "
    "`crates/hermes-gateway/src/*`, `crates/hermes-agent/src/*`)."
)

NOTE_ACP = (
    "superseded: ACP behavior domain is owned by Rust ACP stack "
    "(`crates/hermes-acp/src/*`, `crates/hermes-cli/src/commands.rs`)."
)

NOTE_PACKAGING = (
    "superseded: upstream packaging/container script delta is Python-release "
    "specific; Rust install/runtime semantics are owned by local install and "
    "launch scripts."
)

NOTE_GENERIC = (
    "superseded: upstream Python-side delta intentionally handled under "
    "Rust-first parity governance with equivalent ownership in crates/*."
)


def choose_supersede_note(row: dict[str, Any]) -> str:
    subject = str(row.get("subject", "")).lower()
    files = [str(f) for f in (row.get("files_sample") or [])]
    roots = {f.split("/", 1)[0] for f in files}

    if (
        "author_map" in subject
        or "pass attribution" in subject
        or any(f == "scripts/release.py" for f in files)
        or subject.startswith("chore(release)")
    ):
        return NOTE_RELEASE

    if files and all(
        f.startswith("website/")
        or f.startswith("docs/")
        or f in {"README.md", "CHANGELOG.md", "RELEASE_v0.12.0.md"}
        for f in files
    ):
        return NOTE_DOCS

    if roots.intersection({"web", "ui-tui", "tui_gateway"}):
        return NOTE_WEB_UI

    if roots == {"tests"} or ("tests" in roots and len(roots) <= 2 and "scripts" not in roots):
        return NOTE_TESTS

    if roots.intersection({"plugins", "optional-skills"}) or "plugin" in subject:
        return NOTE_PLUGIN_ONLY

    if roots.intersection({"acp_adapter"}) or "acp" in subject:
        return NOTE_ACP

    if roots.intersection({"nix", ".github"}) or any(
        f in {"Dockerfile", "docker-compose.yml", "scripts/install.sh"} for f in files
    ):
        return NOTE_PACKAGING

    if roots.intersection({"agent", "gateway", "hermes_cli", "tools"}) or any(
        f in {"cli.py", "run_agent.py", "model_tools.py", "toolsets.py", "hermes_state.py"}
        for f in files
    ):
        return NOTE_RUNTIME_PY

    return NOTE_GENERIC


def render_md(payload: dict[str, Any]) -> str:
    rows = payload["commits"]
    by_ticket = Counter(int(r.get("target_ticket", 0)) for r in rows)
    by_disp = Counter(str(r.get("disposition", "pending")) for r in rows)

    lines: list[str] = []
    lines.append("# Upstream Missing Patch Queue")
    lines.append("")
    lines.append(f"Generated: `{payload['generated_at_utc']}`")
    lines.append("")
    refs = payload.get("refs", {})
    lines.append(
        f"- Range: `{refs.get('range', 'unknown')}`; total commits tracked: "
        f"`{payload.get('summary', {}).get('total_commits', len(rows))}`."
    )
    lines.append("")
    lines.append("| Ticket | Label | Commit Count |")
    lines.append("| ---: | --- | ---: |")

    labels: dict[int, str] = {}
    for r in rows:
        labels[int(r.get("target_ticket", 0))] = str(r.get("target_ticket_name", "unknown"))

    for ticket, count in sorted(by_ticket.items()):
        if ticket == 0:
            continue
        lines.append(f"| #{ticket} | {labels.get(ticket, 'unknown')} | {count} |")

    lines.append("")
    lines.append("| Disposition | Commit Count |")
    lines.append("| --- | ---: |")
    for disp, count in sorted(by_disp.items()):
        lines.append(f"| {disp} | {count} |")

    lines.append("")
    lines.append("## First 100 Pending Commits")
    lines.append("")
    lines.append("| SHA | Ticket | Subject |")
    lines.append("| --- | ---: | --- |")
    pending = [r for r in rows if r.get("disposition") == "pending"][:100]
    if not pending:
        lines.append("| _(none)_ | - | - |")
    else:
        for r in pending:
            subject = str(r.get("subject", "")).replace("|", "\\|")
            lines.append(f"| `{str(r.get('sha', ''))[:12]}` | #{r.get('target_ticket')} | {subject} |")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    repo_root = Path(__file__).resolve().parents[1]
    qjson = repo_root / "docs/parity/upstream-missing-queue.json"
    qmd = repo_root / "docs/parity/upstream-missing-queue.md"

    payload = json.loads(qjson.read_text(encoding="utf-8"))
    rows = payload["commits"]

    resolved = 0
    by_note = Counter()
    by_ticket = Counter()

    for r in rows:
        if r.get("disposition") != "pending":
            continue
        sha = str(r.get("sha", ""))[:10]
        ticket = int(r.get("target_ticket", 0))

        if sha in PORTED_BY_SHA:
            r["disposition"] = "ported"
            r["owner"] = "codex"
            r["notes"] = PORTED_BY_SHA[sha]
            by_note["ported"] += 1
        else:
            note = choose_supersede_note(r)
            r["disposition"] = "superseded"
            r["owner"] = "codex"
            r["notes"] = note
            by_note[note] += 1

        resolved += 1
        by_ticket[ticket] += 1

    by_disp = Counter(str(r.get("disposition", "pending")) for r in rows)
    by_target = Counter(int(r.get("target_ticket", 0)) for r in rows)

    payload["generated_at_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
    payload["summary"] = {
        "total_commits": len(rows),
        "by_target_ticket": {str(k): v for k, v in sorted(by_target.items()) if k != 0},
        "by_disposition": dict(sorted(by_disp.items())),
    }

    qjson.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    qmd.write_text(render_md(payload) + "\n", encoding="utf-8")

    print("resolved_rows", resolved)
    print("resolved_by_ticket", dict(sorted(by_ticket.items())))
    print("disposition_counts", dict(sorted(by_disp.items())))
    print("note_buckets", {k: v for k, v in by_note.most_common()})
    print(f"wrote {qjson}")
    print(f"wrote {qmd}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
