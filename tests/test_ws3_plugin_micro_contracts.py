"""Contracts for the small WS3 plugin local-better divergence slice."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


def _read(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def test_disk_cleanup_help_check_uses_local_stable_tuple() -> None:
    text = _read("plugins/disk-cleanup/__init__.py")

    assert 'argv[0] in ("help", "-h", "--help")' in text
    assert 'argv[0] in {"help", "-h", "--help"}' not in text


def test_video_gen_xai_uses_supergrok_subscription_wording() -> None:
    text = _read("plugins/video_gen/xai/__init__.py")

    assert "SuperGrok subscription" in text
    assert "Premium+" not in text


def test_teams_pipeline_membership_checks_use_local_stable_tuples() -> None:
    for path, expected in {
        "plugins/teams_pipeline/cli.py": [
            'action in ("list", "ls")',
            'action in ("run", "replay")',
            'action in ("fetch", "test")',
            'action in ("subscriptions", "subs")',
            'action in ("token-health", "token")',
        ],
        "plugins/teams_pipeline/meetings.py": [
            "exc.status_code in (401, 403)",
        ],
        "plugins/teams_pipeline/models.py": [
            'self.artifact_type not in ("transcript", "recording", "call_record")',
        ],
        "plugins/teams_pipeline/runtime.py": [
            'value not in (None, "")',
        ],
    }.items():
        text = _read(path)
        for needle in expected:
            assert needle in text


def test_langfuse_observability_placeholder_guard_is_present() -> None:
    text = _read("plugins/observability/langfuse/__init__.py")

    assert "_LANGFUSE_KEY_PREFIXES" in text
    assert '"HERMES_LANGFUSE_PUBLIC_KEY": "pk-lf-"' in text
    assert '"HERMES_LANGFUSE_SECRET_KEY": "sk-lf-"' in text
    assert "def _validate_langfuse_key" in text
    assert "credentials look like placeholders" in text


def test_langfuse_trace_serialization_tracks_tool_calls() -> None:
    text = _read("plugins/observability/langfuse/__init__.py")

    assert "pending_tools_by_name" in text
    assert "def _coerce_request_messages" in text
    assert '"function": {' in text
    assert '"tool_call_id"' in text


def test_langfuse_docs_point_to_tools_flow() -> None:
    readme = _read("plugins/observability/langfuse/README.md")
    manifest = _read("plugins/observability/langfuse/plugin.yaml")

    assert "hermes tools" in readme
    assert "Langfuse Observability" in readme
    assert "Langfuse Observability" in manifest
