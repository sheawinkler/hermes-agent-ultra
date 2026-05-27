"""WS3 dashboard plugin divergence contracts."""

from __future__ import annotations

import json
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def _read(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def test_dashboard_plugin_shared_diff_classifications_are_file_scoped() -> None:
    data = json.loads(
        (REPO_ROOT / "docs/parity/shared-different-classification.json").read_text(
            encoding="utf-8",
        ),
    )
    by_path = {item["path"]: item for item in data["items"]}

    for path in {
        "plugins/hermes-achievements/dashboard/dist/index.js",
        "plugins/kanban/dashboard/dist/index.js",
        "plugins/kanban/dashboard/dist/style.css",
        "plugins/kanban/dashboard/plugin_api.py",
    }:
        item = by_path[path]
        assert item["classification"] == "intentional_divergence"
        assert item["owner"] == "sheawinkler"
        assert item["ticket"] == 304


def test_achievements_dashboard_uses_local_static_bundle_contract() -> None:
    bundle = _read("plugins/hermes-achievements/dashboard/dist/index.js")

    assert 'Authorization: `Bearer ${token}`' in bundle
    assert "X-Hermes-Session-Token" not in bundle
    assert "SDK.useI18n" not in bundle
    assert "function tx(" not in bundle
    assert '"Share: " + achievement.name' in bundle


def test_kanban_dashboard_bundle_stays_on_local_legacy_contract() -> None:
    bundle = _read("plugins/kanban/dashboard/dist/index.js")

    assert 'const COLUMN_ORDER = ["triage", "todo", "ready", "running", "blocked", "done"];' in bundle
    assert "SDK.useI18n" not in bundle
    assert "parseApiErrorMessage" not in bundle
    assert "const Checkbox = SDK.components.Checkbox" not in bundle
    assert "hermes-kanban:delete" not in bundle


def test_kanban_dashboard_css_stays_on_local_layout_contract() -> None:
    css = _read("plugins/kanban/dashboard/dist/style.css")

    assert "display: grid;" in css
    assert "grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));" in css
    assert ".hermes-kanban-drag-ghost" not in css
    assert ".hermes-kanban-trash" not in css
    assert ".hermes-kanban-card--failed" not in css


def test_kanban_dashboard_api_does_not_partially_vendor_upstream_python_stack() -> None:
    api = _read("plugins/kanban/dashboard/plugin_api.py")

    assert not (REPO_ROOT / "hermes_cli" / "kanban_db.py").exists()
    assert not (REPO_ROOT / "hermes_cli" / "web_server.py").exists()
    assert 'BOARD_COLUMNS: list[str] = [\n    "triage", "todo", "ready", "running", "blocked", "done",\n]' in api
    assert "workflow_template_id" not in api
    assert "current_step_key" not in api
    assert "kanban_db.schedule_task" not in api
    assert '@router.delete("/tasks/{task_id}")' not in api
