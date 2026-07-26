from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-upstream-workstreams.py"
spec = importlib.util.spec_from_file_location("generate_upstream_workstreams", SCRIPT)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def _commit(sha_char: str, subject: str, files: list[str]) -> dict[str, object]:
    return {
        "sha": sha_char * 40,
        "subject": subject,
        "files": files,
        "files_touched": len(files),
        "disposition": "pending",
        "target_ticket": 23,
    }


def test_classifies_specific_runtime_surfaces_before_generic_agent_paths() -> None:
    assert (
        module.classify_workstream(
            _commit(
                "a", "fix(model): preserve provider usage", ["agent/model/provider.py"]
            )
        )
        == "model-providers"
    )
    assert (
        module.classify_workstream(
            _commit(
                "b", "fix(session): persist resume state", ["agent/session_store.py"]
            )
        )
        == "sessions-checkpoints-worktrees"
    )
    assert (
        module.classify_workstream(
            _commit("c", "fix: execute tool safely", ["agent/tool_execution.py"])
        )
        == "tools-mcp-acp"
    )


def test_routes_model_provider_plugins_before_generic_plugins() -> None:
    assert (
        module.classify_workstream(
            _commit(
                "5",
                "feat(provider): add Upstage",
                ["plugins/model-providers/upstage/provider.py"],
            )
        )
        == "model-providers"
    )


def test_routes_cron_paths_to_scheduler_before_cli_or_plugins() -> None:
    assert (
        module.classify_workstream(
            _commit(
                "3",
                "fix(cron): verify Chronos provider",
                ["plugins/cron_providers/chronos/verify.py"],
            )
        )
        == "scheduler-delegation"
    )
    assert (
        module.classify_workstream(
            _commit("4", "feat(cron): add status", ["hermes_cli/cron.py"])
        )
        == "scheduler-delegation"
    )


def test_routes_shared_app_and_repository_metadata() -> None:
    assert (
        module.classify_workstream(
            _commit("6", "fix(shared): add script alias", ["apps/shared/package.json"])
        )
        == "desktop-app"
    )
    assert (
        module.classify_workstream(
            _commit("7", "chore: ignore node modules", [".gitignore"])
        )
        == "release-packaging-build"
    )


def test_runtime_path_wins_over_docs_and_metadata_companions() -> None:
    mixed = _commit(
        "8",
        "feat(delegation): let subagents use execute_code",
        [
            "AGENTS.md",
            "tests/tools/test_delegate.py",
            "tools/delegate_tool.py",
            "website/docs/guides/delegation-patterns.md",
        ],
    )
    docs_only = _commit("9", "docs: update agent guidance", ["AGENTS.md"])

    assert module.classify_workstream(mixed) == "scheduler-delegation"
    assert module.classify_workstream(docs_only) == "docs-localization"


def test_code_path_wins_over_companion_test_path() -> None:
    commit = _commit(
        "d",
        "fix(discord): recover missed messages",
        ["gateway/platforms/discord.py", "tests/gateway/test_discord.py"],
    )

    assert module.classify_workstream(commit) == "gateway-platforms"


def test_build_plan_assigns_every_pending_sha_once() -> None:
    pending = [
        _commit("e", "fix(agent): retry stream", ["agent/agent_loop.py"]),
        _commit(
            "f", "feat(desktop): restore layout", ["apps/desktop/src/app/layout.tsx"]
        ),
        _commit("1", "test: add release fixture", ["tests/test_release.py"]),
    ]
    queue = {
        "generated_at_utc": "2026-07-25T00:00:00+00:00",
        "commits": [
            *pending,
            {
                **_commit("2", "docs: explain parity", ["docs/parity.md"]),
                "disposition": "superseded",
            },
        ],
    }

    plan = module.build_plan(queue, hashlib.sha256(b"queue").hexdigest())

    assigned = [
        commit["sha"]
        for workstream in plan["workstreams"]
        for commit in workstream["commits"]
    ]
    assert plan["pending_count"] == 3
    assert plan["truth"] == {
        "pass": True,
        "all_pending_assigned_once": True,
        "missing_shas": [],
        "extra_shas": [],
        "duplicate_assignments": [],
    }
    assert sorted(assigned) == sorted(commit["sha"] for commit in pending)


def test_build_plan_rejects_duplicate_pending_sha() -> None:
    duplicate = _commit("a", "fix(agent): retry stream", ["agent/agent_loop.py"])

    try:
        module.build_plan({"commits": [duplicate, duplicate]}, "digest")
    except ValueError as exc:
        assert "duplicate SHAs" in str(exc)
    else:
        raise AssertionError("duplicate pending SHA was accepted")
