#!/usr/bin/env python3
"""Run a lightweight live-session evaluation harness from saved Hermes sessions."""

from __future__ import annotations

import argparse
import json
import pathlib
from dataclasses import dataclass
from datetime import datetime, timezone
from statistics import median
from typing import Any


def utc_now_stamp() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run live session evaluation harness.")
    parser.add_argument("--repo-root", default=".", help="Repository root path (default: .)")
    parser.add_argument(
        "--sessions-dir",
        default=None,
        help="Optional explicit sessions directory (defaults to ~/.hermes-agent-ultra/sessions)",
    )
    parser.add_argument("--max-sessions", type=int, default=25, help="Max sessions to inspect")
    parser.add_argument(
        "--out-json",
        default=None,
        help="Optional output path (default: .sync-reports/session-eval-harness-<ts>.json)",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON to stdout")
    return parser.parse_args()


def resolve_sessions_dir(explicit: str | None) -> pathlib.Path:
    if explicit:
        return pathlib.Path(explicit).expanduser().resolve()
    home = pathlib.Path.home()
    return (home / ".hermes-agent-ultra" / "sessions").resolve()


def normalize_role(raw: str | None) -> str:
    value = str(raw or "").strip().lower()
    if value in {"assistant", "user", "system", "tool"}:
        return value
    return "unknown"


def text_has_tool_markers(text: str) -> bool:
    lower = text.lower()
    markers = [
        "<tool_call",
        "<tool_use",
        "tool_call",
        "\"tool\":",
        "\"tool_name\":",
        "`tool`",
    ]
    return any(marker in lower for marker in markers)


def text_has_patch_markers(text: str) -> bool:
    lower = text.lower()
    markers = ["[objective_patch]", "exists_now=true", "verified_exists=true", "apply_patch"]
    return any(marker in lower for marker in markers)


@dataclass
class SessionStats:
    name: str
    message_count: int
    user_count: int
    assistant_count: int
    has_tool_activity: bool
    has_objective_activity: bool
    has_patch_evidence: bool
    modified_at: str


def load_session_stats(path: pathlib.Path) -> SessionStats | None:
    try:
        raw = path.read_text(encoding="utf-8")
        doc = json.loads(raw)
    except Exception:
        return None
    messages = doc.get("messages")
    if not isinstance(messages, list):
        return None
    user_count = 0
    assistant_count = 0
    has_tool = False
    has_objective = False
    has_patch = False
    for msg in messages:
        if not isinstance(msg, dict):
            continue
        role = normalize_role(msg.get("role"))
        content = str(msg.get("content") or "")
        if role == "user":
            user_count += 1
        elif role == "assistant":
            assistant_count += 1
        if text_has_tool_markers(content):
            has_tool = True
        if "/objective" in content.lower() or "[objective_" in content.lower():
            has_objective = True
        if text_has_patch_markers(content):
            has_patch = True
    modified_at = datetime.fromtimestamp(path.stat().st_mtime, tz=timezone.utc).isoformat()
    return SessionStats(
        name=path.stem,
        message_count=len(messages),
        user_count=user_count,
        assistant_count=assistant_count,
        has_tool_activity=has_tool,
        has_objective_activity=has_objective,
        has_patch_evidence=has_patch,
        modified_at=modified_at,
    )


def load_latest_sessions(sessions_dir: pathlib.Path, max_sessions: int) -> list[SessionStats]:
    if not sessions_dir.exists():
        return []
    files = [p for p in sessions_dir.glob("*.json") if p.is_file()]
    files.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    stats: list[SessionStats] = []
    for path in files[: max(1, max_sessions)]:
        parsed = load_session_stats(path)
        if parsed is not None:
            stats.append(parsed)
    return stats


def build_report(
    repo_root: pathlib.Path, sessions_dir: pathlib.Path, sessions: list[SessionStats]
) -> dict[str, Any]:
    counts = [s.message_count for s in sessions]
    avg_messages = (sum(counts) / len(counts)) if counts else 0.0
    med_messages = median(counts) if counts else 0.0
    tool_sessions = sum(1 for s in sessions if s.has_tool_activity)
    objective_sessions = sum(1 for s in sessions if s.has_objective_activity)
    patch_sessions = sum(1 for s in sessions if s.has_patch_evidence)
    user_turns = sum(s.user_count for s in sessions)
    assistant_turns = sum(s.assistant_count for s in sessions)
    latest = sessions[0].modified_at if sessions else None

    ok = bool(
        sessions
        and avg_messages >= 2.0
        and assistant_turns >= user_turns
        and tool_sessions >= max(1, len(sessions) // 5)
    )
    reasons: list[str] = []
    if not sessions:
        reasons.append("no_saved_sessions")
    if avg_messages < 2.0:
        reasons.append("avg_messages_too_low")
    if assistant_turns < user_turns:
        reasons.append("assistant_turns_below_user_turns")
    if tool_sessions < max(1, len(sessions) // 5):
        reasons.append("low_tool_activity_ratio")

    return {
        "ok": ok,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "sessions_dir": str(sessions_dir),
        "summary": {
            "sessions_analyzed": len(sessions),
            "avg_messages_per_session": round(avg_messages, 2),
            "median_messages_per_session": round(float(med_messages), 2),
            "tool_activity_sessions": tool_sessions,
            "objective_activity_sessions": objective_sessions,
            "patch_evidence_sessions": patch_sessions,
            "user_turns": user_turns,
            "assistant_turns": assistant_turns,
            "latest_session_modified_at": latest,
        },
        "reasons": reasons,
        "sessions": [
            {
                "name": s.name,
                "message_count": s.message_count,
                "user_count": s.user_count,
                "assistant_count": s.assistant_count,
                "has_tool_activity": s.has_tool_activity,
                "has_objective_activity": s.has_objective_activity,
                "has_patch_evidence": s.has_patch_evidence,
                "modified_at": s.modified_at,
            }
            for s in sessions[:10]
        ],
    }


def main() -> int:
    args = parse_args()
    repo_root = pathlib.Path(args.repo_root).expanduser().resolve()
    sessions_dir = resolve_sessions_dir(args.sessions_dir)
    sessions = load_latest_sessions(sessions_dir, args.max_sessions)
    report = build_report(repo_root, sessions_dir, sessions)

    out_path = (
        pathlib.Path(args.out_json).expanduser().resolve()
        if args.out_json
        else repo_root / ".sync-reports" / f"session-eval-harness-{utc_now_stamp()}.json"
    )
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if args.json:
        print(json.dumps(report, indent=2))
        print(f"report_path={out_path}")
    else:
        status = "PASS" if report.get("ok") else "FAIL"
        summary = report.get("summary", {})
        print(
            f"[session-eval-harness] {status} sessions={summary.get('sessions_analyzed', 0)} "
            f"avg_msgs={summary.get('avg_messages_per_session', 0)} "
            f"tool_sessions={summary.get('tool_activity_sessions', 0)}"
        )
        print(f"[session-eval-harness] Report: {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
