"""Contracts for the Rust Docker image packaging surface."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = REPO_ROOT / "Dockerfile"
DOCKER_DOC = REPO_ROOT / "website" / "docs" / "user-guide" / "docker.md"


def test_docker_image_carries_license_metadata_and_files() -> None:
    text = DOCKERFILE.read_text()

    assert 'org.opencontainers.image.licenses="MIT"' in text
    assert "COPY LICENSE NOTICE /usr/share/doc/hermes-agent-ultra/" in text
    assert "chmod -R a+rX /usr/local/bin /usr/share/doc/hermes-agent-ultra" in text


def test_docker_context_does_not_exclude_license_file() -> None:
    dockerignore = REPO_ROOT / ".dockerignore"
    if not dockerignore.exists():
        return

    active_lines = [
        line.strip()
        for line in dockerignore.read_text().splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]

    assert "LICENSE" not in active_lines
    assert "NOTICE" not in active_lines


def test_docker_image_has_no_python_setup_install_surface() -> None:
    text = DOCKERFILE.read_text()

    assert not (REPO_ROOT / "setup.py").exists()
    assert "pip install" not in text
    assert "uv pip install" not in text
    assert "setup.py" not in text


def test_docker_image_keeps_code_immutable_and_state_on_data_volume() -> None:
    text = DOCKERFILE.read_text()

    assert "COPY --from=builder /app/target/release/hermes /usr/local/bin/hermes" in text
    assert "ENV HERMES_HOME=/data" in text
    assert 'VOLUME ["/data"]' in text
    assert "chown -R 10000:10000 /data" in text
    assert "chown -R 10000:10000 /usr/local" not in text


def test_docker_docs_match_ultra_image_and_data_volume() -> None:
    text = DOCKER_DOC.read_text()

    assert "hermes-agent-ultra" in text
    assert "nousresearch/hermes-agent" not in text
    assert "/opt/data" not in text
    assert "HERMES_DASHBOARD=1" not in text
