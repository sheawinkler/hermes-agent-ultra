#!/usr/bin/env python3
"""Group every pending upstream parity commit into one deterministic workstream."""

from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

SHA_RE = re.compile(r"^[0-9a-f]{40}$")

WORKSTREAMS: tuple[dict[str, Any], ...] = (
    {
        "id": "core-agent-runtime",
        "priority": 1,
        "description": "Agent loop, streaming, retries, execution policy, and runtime state.",
        "dependencies": [],
        "prefixes": ("agent/", "run_agent.py"),
        "terms": ("agent loop", "stream", "retry", "runtime", "inference", "message"),
    },
    {
        "id": "config-secrets-auth",
        "priority": 2,
        "description": "Configuration, credentials, secrets, OAuth, and authentication.",
        "dependencies": ["core-agent-runtime"],
        "prefixes": (
            ".env",
            "agent/config",
            "agent/auth",
            "hermes_cli/config",
            "hermes_cli/auth",
            "cli-config",
        ),
        "terms": (
            "config",
            "secret",
            "credential",
            "oauth",
            "auth",
            "keychain",
            "token",
        ),
    },
    {
        "id": "model-providers",
        "priority": 3,
        "description": "Model routing, provider adapters, schemas, and usage accounting.",
        "dependencies": ["core-agent-runtime", "config-secrets-auth"],
        "prefixes": (
            "agent/model",
            "agent/provider",
            "hermes_cli/providers",
            "model_tools.py",
            "plugins/model-providers/",
            "providers/",
        ),
        "terms": (
            "provider",
            "model",
            "anthropic",
            "openai",
            "bedrock",
            "gemini",
            "ollama",
            "vertex",
            "moa",
        ),
    },
    {
        "id": "sessions-checkpoints-worktrees",
        "priority": 4,
        "description": "Session persistence, context, checkpoints, replay, and worktrees.",
        "dependencies": ["core-agent-runtime", "config-secrets-auth"],
        "prefixes": (
            "agent/session",
            "agent/context",
            "agent/checkpoint",
            "hermes_state.py",
        ),
        "terms": ("session", "checkpoint", "context", "worktree", "replay", "resume"),
    },
    {
        "id": "tools-mcp-acp",
        "priority": 5,
        "description": "Tool execution, MCP, ACP, browser, terminal, and computer-use surfaces.",
        "dependencies": ["core-agent-runtime", "config-secrets-auth"],
        "prefixes": (
            "agent/acp",
            "agent/mcp",
            "agent/tool",
            "tools/",
            "acp_adapter/",
            "acp_registry/",
            "toolsets.py",
        ),
        "terms": (
            "tool",
            "mcp",
            "acp",
            "browser",
            "terminal",
            "computer use",
            "sandbox",
        ),
    },
    {
        "id": "gateway-platforms",
        "priority": 6,
        "description": "Gateway lifecycle and Discord, Slack, Telegram, WhatsApp, and relay adapters.",
        "dependencies": [
            "core-agent-runtime",
            "config-secrets-auth",
            "sessions-checkpoints-worktrees",
            "tools-mcp-acp",
        ],
        "prefixes": ("gateway/", "plugins/platforms/"),
        "terms": (
            "gateway",
            "discord",
            "slack",
            "telegram",
            "whatsapp",
            "relay",
            "voice",
        ),
    },
    {
        "id": "cli-tui",
        "priority": 7,
        "description": "CLI commands, TUI state, rendering, and interactive workflows.",
        "dependencies": [
            "core-agent-runtime",
            "config-secrets-auth",
            "sessions-checkpoints-worktrees",
            "tools-mcp-acp",
        ],
        "prefixes": ("hermes_cli/", "cli.py", "ui-tui/", "tui_gateway/"),
        "terms": ("cli", "tui", "command", "prompt", "keyboard", "render"),
    },
    {
        "id": "scheduler-delegation",
        "priority": 8,
        "description": "Cron, scheduling, delegation, subagents, and background work.",
        "dependencies": [
            "core-agent-runtime",
            "sessions-checkpoints-worktrees",
            "tools-mcp-acp",
        ],
        "prefixes": (
            "agent/cron",
            "agent/deleg",
            "agent/scheduler",
            "cron/",
            "hermes_cli/cron",
            "plugins/cron/",
            "plugins/cron_providers/",
        ),
        "terms": ("cron", "schedule", "delegat", "subagent", "background", "worker"),
    },
    {
        "id": "memory-plugins-skills",
        "priority": 9,
        "description": "Memory providers, plugin loading, skills, and optional capabilities.",
        "dependencies": ["core-agent-runtime", "config-secrets-auth", "tools-mcp-acp"],
        "prefixes": (
            "plugins/memory/",
            "hermes_cli/memory",
            "plugins/",
            "skills/",
            "optional-skills/",
        ),
        "terms": ("memory", "honcho", "mem0", "plugin", "skill"),
    },
    {
        "id": "desktop-app",
        "priority": 10,
        "description": "Electron desktop runtime, IPC, session UI, settings, and packaging.",
        "dependencies": [
            "cli-tui",
            "gateway-platforms",
            "sessions-checkpoints-worktrees",
        ],
        "prefixes": ("apps/desktop/", "apps/bootstrap-installer/"),
        "terms": ("desktop", "electron", "ipc"),
    },
    {
        "id": "web-dashboard-website",
        "priority": 11,
        "description": "Web server, dashboard, website, and browser-facing APIs.",
        "dependencies": [
            "cli-tui",
            "gateway-platforms",
            "sessions-checkpoints-worktrees",
        ],
        "prefixes": ("web/", "website/", "hermes_cli/web_server.py"),
        "terms": ("dashboard", "website", "web server", "websocket", "http"),
    },
    {
        "id": "release-packaging-build",
        "priority": 12,
        "description": "Release automation, packaging, installers, CI, and dependency metadata.",
        "dependencies": [],
        "prefixes": (
            ".github/",
            "scripts/",
            "contributors/",
            "nix/",
            "docker/",
            "pyproject.toml",
            "uv.lock",
            "package.json",
            "package-lock.json",
            "MANIFEST.in",
        ),
        "terms": ("release", "package", "build", "ci", "installer", "author_map"),
    },
    {
        "id": "test-harness-quality",
        "priority": 13,
        "description": "Tests, fixtures, harnesses, linting, formatting, and quality gates.",
        "dependencies": [],
        "prefixes": ("tests/",),
        "terms": ("test", "fixture", "harness", "lint", "format", "coverage"),
    },
    {
        "id": "docs-localization",
        "priority": 14,
        "description": "Documentation, examples, localization, and contributor guidance.",
        "dependencies": [],
        "prefixes": ("docs/", "locales/", "AGENTS.md", "README"),
        "terms": ("docs", "readme", "localization", "translation"),
    },
    {
        "id": "miscellaneous-unmapped",
        "priority": 15,
        "description": "Unmapped commits requiring explicit subsystem reassignment before closure.",
        "dependencies": [],
        "prefixes": (),
        "terms": (),
    },
)

IGNORED_PRIMARY_PREFIXES = ("tests/", "docs/", "locales/", ".github/")


def _normalized_paths(commit: dict[str, Any]) -> list[str]:
    raw = commit.get("files", commit.get("files_sample", []))
    if not isinstance(raw, list):
        raise ValueError(
            f"commit {commit.get('sha', '<unknown>')} files must be an array"
        )
    return sorted(
        {str(path).strip().replace("\\", "/") for path in raw if str(path).strip()}
    )


def _score_rule(rule: dict[str, Any], paths: list[str], subject: str) -> int:
    prefix_score = max(
        (
            len(prefix) * 100
            for path in paths
            for prefix in rule["prefixes"]
            if path.startswith(prefix)
        ),
        default=0,
    )
    subject_score = sum(10 for term in rule["terms"] if term in subject)
    return prefix_score + subject_score


def classify_workstream(commit: dict[str, Any]) -> str:
    paths = _normalized_paths(commit)
    primary_paths = [
        path
        for path in paths
        if not any(path.startswith(prefix) for prefix in IGNORED_PRIMARY_PREFIXES)
    ]
    scored_paths = primary_paths or paths
    subject = str(commit.get("subject", "")).lower()
    candidates = []
    for rule in WORKSTREAMS[:-1]:
        score = _score_rule(rule, scored_paths, subject)
        if score:
            candidates.append((score, -int(rule["priority"]), str(rule["id"])))
    return max(candidates)[2] if candidates else "miscellaneous-unmapped"


def build_plan(queue: dict[str, Any], source_digest: str) -> dict[str, Any]:
    commits = queue.get("commits")
    if not isinstance(commits, list):
        raise ValueError("queue must contain a commits array")
    pending = [row for row in commits if row.get("disposition") == "pending"]
    pending_shas = [str(row.get("sha", "")) for row in pending]
    if any(not SHA_RE.fullmatch(sha) for sha in pending_shas):
        raise ValueError(
            "every pending commit must have an exact lowercase 40-character SHA"
        )
    duplicate_shas = sorted(
        sha for sha, count in Counter(pending_shas).items() if count > 1
    )
    if duplicate_shas:
        raise ValueError(f"pending queue contains duplicate SHAs: {duplicate_shas[:5]}")

    grouped: dict[str, list[dict[str, Any]]] = {rule["id"]: [] for rule in WORKSTREAMS}
    for commit in pending:
        workstream_id = classify_workstream(commit)
        grouped[workstream_id].append(
            {
                "sha": str(commit["sha"]),
                "subject": str(commit.get("subject", "")),
                "files": _normalized_paths(commit),
                "files_touched": int(commit.get("files_touched", 0)),
                "target_ticket": commit.get("target_ticket"),
            }
        )

    assigned_shas = [row["sha"] for rows in grouped.values() for row in rows]
    missing = sorted(set(pending_shas) - set(assigned_shas))
    extra = sorted(set(assigned_shas) - set(pending_shas))
    assignment_counts = Counter(assigned_shas)
    duplicated_assignments = sorted(
        sha for sha, count in assignment_counts.items() if count != 1
    )
    truth_pass = (
        not missing
        and not extra
        and not duplicated_assignments
        and len(assigned_shas) == len(pending)
    )

    workstreams = []
    for rule in WORKSTREAMS:
        rows = grouped[rule["id"]]
        workstreams.append(
            {
                "id": rule["id"],
                "priority": rule["priority"],
                "description": rule["description"],
                "dependencies": rule["dependencies"],
                "commit_count": len(rows),
                "commits": rows,
            }
        )

    source_ledger = queue.get("source_ledger", {})
    return {
        "schema_version": 1,
        "source": {
            "queue": "docs/parity/upstream-missing-queue.json",
            "pending_sha256": source_digest,
            "upstream_sha": source_ledger.get("upstream_sha"),
            "range_sha256": source_ledger.get("range_sha256"),
        },
        "pending_count": len(pending),
        "workstreams": workstreams,
        "truth": {
            "pass": truth_pass,
            "all_pending_assigned_once": truth_pass,
            "missing_shas": missing,
            "extra_shas": extra,
            "duplicate_assignments": duplicated_assignments,
        },
    }


def pending_source_digest(queue: dict[str, Any]) -> str:
    commits = queue.get("commits")
    if not isinstance(commits, list):
        raise ValueError("queue must contain a commits array")
    projection = [
        {
            "sha": str(commit.get("sha", "")),
            "subject": str(commit.get("subject", "")),
            "files": _normalized_paths(commit),
            "files_touched": int(commit.get("files_touched", 0)),
            "target_ticket": commit.get("target_ticket"),
        }
        for commit in commits
        if commit.get("disposition") == "pending"
    ]
    canonical = json.dumps(projection, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--queue", type=Path, default=Path("docs/parity/upstream-missing-queue.json")
    )
    parser.add_argument(
        "--out-json", type=Path, default=Path("docs/parity/upstream-workstreams.json")
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Fail if the checked-in workstream plan is stale",
    )
    args = parser.parse_args()

    raw = args.queue.read_bytes()
    queue = json.loads(raw)
    plan = build_plan(queue, pending_source_digest(queue))
    rendered = json.dumps(plan, indent=2, sort_keys=True) + "\n"
    if args.check:
        try:
            current = args.out_json.read_text(encoding="utf-8")
        except OSError as exc:
            print(f"cannot read {args.out_json}: {exc}", file=sys.stderr)
            return 1
        if current != rendered:
            print(f"stale workstream plan: regenerate {args.out_json}", file=sys.stderr)
            return 1
    else:
        args.out_json.parent.mkdir(parents=True, exist_ok=True)
        args.out_json.write_text(rendered, encoding="utf-8")
        print(f"Wrote {args.out_json}")
    return 0 if plan["truth"]["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
