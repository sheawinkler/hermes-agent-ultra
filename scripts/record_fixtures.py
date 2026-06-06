#!/usr/bin/env python3
"""Regenerate or verify golden outputs from `research/hermes-agent` (Python).

Usage (from `hermes-agent-rust` repo root)::

    python3 scripts/record_fixtures.py

Requires a checkout of Hermes Python next to this repo::

    research/hermes-agent-rust/   (this repo)
    research/hermes-agent/        (Python package root: contains `agent/`)

The script imports `agent.anthropic_adapter` when available and prints JSON
blobs that should match `crates/hermes-parity-tests/fixtures/**/*.json`.

Always-emitted cases (no Python package needed): checkpoint-manager pure
helpers (`checkpoint_shadow_dir_id`, commit-hash validation, shortstat parsing).

With `research/hermes-agent`: also emits Anthropic adapter cases
(`normalize_model_name`, `_sanitize_tool_id`, `_is_oauth_token`,
`_common_betas_for_base_url`, OAuth beta list).
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def python_agent_root() -> Path:
    return repo_root().parent / "hermes-agent"


def checkpoint_shadow_dir_id(abs_path: str) -> str:
    """Match Rust `hermes_parity_tests::harness::checkpoint_shadow_dir_id`."""
    return hashlib.sha256(abs_path.encode()).hexdigest()[:16]


_COMMIT_HASH_RE = re.compile(r"^[0-9a-fA-F]{4,64}$")


def checkpoint_validate_commit_hash(commit_hash: str) -> str | None:
    """Match Python `tools.checkpoint_manager._validate_commit_hash`."""
    if not commit_hash or not commit_hash.strip():
        return "Empty commit hash"
    if commit_hash.startswith("-"):
        return f"Invalid commit hash (must not start with '-'): {commit_hash!r}"
    if not _COMMIT_HASH_RE.match(commit_hash):
        return f"Invalid commit hash (expected 4-64 hex characters): {commit_hash!r}"
    return None


def checkpoint_parse_shortstat(stat_line: str) -> dict[str, int]:
    """Match Python `CheckpointManager._parse_shortstat`."""
    entry = {"files_changed": 0, "insertions": 0, "deletions": 0}
    match = re.search(r"(\d+) file", stat_line)
    if match:
        entry["files_changed"] = int(match.group(1))
    match = re.search(r"(\d+) insertion", stat_line)
    if match:
        entry["insertions"] = int(match.group(1))
    match = re.search(r"(\d+) deletion", stat_line)
    if match:
        entry["deletions"] = int(match.group(1))
    return entry


def main() -> int:
    py_root = python_agent_root()
    agent_pkg = py_root / "agent" / "anthropic_adapter.py"

    cases = [
        {
            "op": "checkpoint_shadow_dir_id",
            "input": {"abs_path": "/workspace/demo"},
            "py": checkpoint_shadow_dir_id("/workspace/demo"),
        },
        {
            "op": "checkpoint_validate_commit_hash",
            "input": {"commit_hash": "abc"},
            "py": checkpoint_validate_commit_hash("abc"),
        },
        {
            "op": "checkpoint_validate_commit_hash",
            "input": {"commit_hash": "-abc123"},
            "py": checkpoint_validate_commit_hash("-abc123"),
        },
        {
            "op": "checkpoint_validate_commit_hash",
            "input": {"commit_hash": "ABCDEF1234"},
            "py": checkpoint_validate_commit_hash("ABCDEF1234"),
        },
        {
            "op": "checkpoint_parse_shortstat",
            "input": {"stat_line": "1 file changed, 2 insertions(+), 3 deletions(-)"},
            "py": checkpoint_parse_shortstat(
                "1 file changed, 2 insertions(+), 3 deletions(-)"
            ),
        },
        {
            "op": "checkpoint_parse_shortstat",
            "input": {"stat_line": "2 files changed, 10 insertions(+)"},
            "py": checkpoint_parse_shortstat("2 files changed, 10 insertions(+)"),
        },
    ]

    if not agent_pkg.is_file():
        print(
            f"Note: Python Hermes not found at {py_root} — only checkpoint hash cases emitted.",
            file=sys.stderr,
        )
        out = {"source": str(py_root), "cases": cases}
        print(json.dumps(out, indent=2))
        return 0

    sys.path.insert(0, str(py_root))

    from agent.anthropic_adapter import (  # type: ignore[import-not-found]
        _common_betas_for_base_url,
        _is_oauth_token,
        _sanitize_tool_id,
        normalize_model_name,
    )

    cases.extend(
        [
            {
                "op": "normalize_model_name",
                "input": {"model": "anthropic/claude-opus-4.6", "preserve_dots": False},
                "py": normalize_model_name("anthropic/claude-opus-4.6", False),
            },
            {
                "op": "normalize_model_name",
                "input": {"model": "anthropic/qwen3.5-plus", "preserve_dots": True},
                "py": normalize_model_name("anthropic/qwen3.5-plus", True),
            },
            {
                "op": "sanitize_tool_id",
                "input": {"tool_id": "abc.123"},
                "py": _sanitize_tool_id("abc.123"),
            },
            {
                "op": "sanitize_tool_id",
                "input": {"tool_id": ""},
                "py": _sanitize_tool_id(""),
            },
            {
                "op": "is_oauth_token",
                "input": {"key": "sk-ant-api03-xxx"},
                "py": _is_oauth_token("sk-ant-api03-xxx"),
            },
            {
                "op": "is_oauth_token",
                "input": {"key": "sk-ant-oat-xxx"},
                "py": _is_oauth_token("sk-ant-oat-xxx"),
            },
            {
                "op": "common_betas_for_base_url",
                "input": {"base_url": None},
                "py": _common_betas_for_base_url(None),
            },
            {
                "op": "common_betas_for_base_url",
                "input": {"base_url": "https://api.minimax.io/anthropic/v1"},
                "py": _common_betas_for_base_url("https://api.minimax.io/anthropic/v1"),
            },
        ]
    )

    # OAuth-combined list (matches Rust `default_anthropic_beta_list(..., True)`).
    from agent import anthropic_adapter as aa  # type: ignore[import-not-found]

    oauth_betas = list(aa._common_betas_for_base_url(None)) + list(aa._OAUTH_ONLY_BETAS)
    cases.append(
        {
            "op": "default_anthropic_beta_list",
            "input": {"base_url": None, "is_oauth": True},
            "py": oauth_betas,
        }
    )

    out = {"source": str(py_root), "cases": cases}
    print(json.dumps(out, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
