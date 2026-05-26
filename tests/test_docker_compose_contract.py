"""Contracts for the Ultra docker-compose defaults."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
COMPOSE = REPO_ROOT / "docker-compose.yml"


def test_compose_uses_ultra_image_and_data_volume() -> None:
    text = COMPOSE.read_text()

    assert "image: hermes-agent-ultra" in text
    assert "container_name: hermes-agent-ultra" in text
    assert "- ~/.hermes-agent-ultra:/data" in text
    assert "HERMES_UID=${HERMES_UID:-10000}" in text
    assert "HERMES_GID=${HERMES_GID:-10000}" in text


def test_compose_keeps_dashboard_localhost_by_default() -> None:
    text = COMPOSE.read_text()

    assert 'command: ["dashboard", "--host", "127.0.0.1", "--no-open"]' in text
    assert "# - API_SERVER_HOST=0.0.0.0" in text
    assert "# - API_SERVER_KEY=${API_SERVER_KEY}" in text
