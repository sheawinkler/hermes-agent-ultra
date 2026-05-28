"""README contracts for install and Rust-primary positioning."""

from __future__ import annotations

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
README = REPO_ROOT / "README.md"


def test_readme_points_to_ultra_installer() -> None:
    text = README.read_text()

    assert (
        "https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh"
        in text
    )
    assert "cargo install --git https://github.com/sheawinkler/hermes-agent-ultra" in text
    assert "--bin hermes-agent-ultra --bin hermes-ultra" in text
    assert "--bin hermes\n" not in text


def test_readme_documents_rust_primary_runtime() -> None:
    text = README.read_text()

    assert "Rust-first autonomous agent runtime" in text
    assert "Fully Rust-native core runtime" in text
    assert "Rust-only implementation strategy" in text


def test_readme_quick_start_uses_ultra_commands() -> None:
    text = README.read_text()

    for command in (
        "hermes-ultra setup",
        "hermes-ultra",
        'hermes-ultra chat --query "summarize this repository"',
        "hermes-ultra gateway --live",
        "hermes-ultra doctor --deep --snapshot --bundle",
    ):
        assert command in text
