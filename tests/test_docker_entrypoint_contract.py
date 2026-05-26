"""Contracts for docker/entrypoint.sh."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
ENTRYPOINT = REPO_ROOT / "docker" / "entrypoint.sh"


def test_entrypoint_prepares_hermes_home() -> None:
    text = ENTRYPOINT.read_text()

    assert "set -eu" in text
    assert 'APP_HOME="${HERMES_HOME:-/data}"' in text
    assert 'mkdir -p "${APP_HOME}"' in text


def test_entrypoint_remaps_hermes_user_when_root() -> None:
    text = ENTRYPOINT.read_text()

    assert 'if [ "$(id -u)" = "0" ]; then' in text
    assert 'TARGET_UID="${HERMES_UID:-10000}"' in text
    assert 'TARGET_GID="${HERMES_GID:-${TARGET_UID}}"' in text
    assert 'groupmod -o -g "${TARGET_GID}" hermes' in text
    assert 'usermod -o -u "${TARGET_UID}" -g "${TARGET_GID}" hermes' in text
    assert 'exec gosu "${actual_uid}:${actual_gid}" "$@"' in text
    assert 'exec "$@"' in text
