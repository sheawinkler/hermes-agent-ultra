"""Contracts for Hermes Agent Ultra install-facing documentation."""

from __future__ import annotations

import re
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DOC_ROOTS = [
    REPO_ROOT / "website" / "docs",
    REPO_ROOT / "website" / "scripts",
    REPO_ROOT / "skills",
    REPO_ROOT / "plugins" / "hermes-achievements" / "README.md",
]
PYTHON_LIBRARY_GUIDE = REPO_ROOT / "website" / "docs" / "guides" / "python-library.md"

UPSTREAM_INSTALL_PATTERNS = (
    "raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh",
    "raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.ps1",
)
ULTRA_INSTALL_URL = (
    "https://raw.githubusercontent.com/sheawinkler/hermes-agent-ultra/main/scripts/install.sh"
)

PIP_APP_INSTALL_RE = re.compile(
    r"\bpip\s+install\s+['\"]?hermes-agent(?:\[[^\]]+\])?\b"
)
UV_APP_INSTALL_RE = re.compile(
    r"\buv\s+pip\s+install\b[^\n]*\bhermes-agent(?:\[[^\]]+\])?\b"
)


def iter_install_contract_files() -> list[Path]:
    files: list[Path] = []
    for root in DOC_ROOTS:
        if root.is_file():
            files.append(root)
            continue
        files.extend(
            p
            for p in root.rglob("*")
            if p.suffix in {".md", ".py"} and p != PYTHON_LIBRARY_GUIDE
        )
    return sorted(set(files))


def test_docs_do_not_point_installers_at_upstream_repo() -> None:
    offenders: list[str] = []
    for path in iter_install_contract_files():
        text = path.read_text()
        for pattern in UPSTREAM_INSTALL_PATTERNS:
            if pattern in text:
                offenders.append(f"{path.relative_to(REPO_ROOT)}: {pattern}")

    assert offenders == []


def test_docs_do_not_recommend_pip_installing_the_ultra_app() -> None:
    offenders: list[str] = []
    for path in iter_install_contract_files():
        text = path.read_text()
        if PIP_APP_INSTALL_RE.search(text) or UV_APP_INSTALL_RE.search(text):
            offenders.append(str(path.relative_to(REPO_ROOT)))

    assert offenders == []


def test_install_docs_use_ultra_commands_and_coexistence_contract() -> None:
    install = (REPO_ROOT / "website" / "docs" / "getting-started" / "installation.md").read_text()
    quickstart = (REPO_ROOT / "website" / "docs" / "getting-started" / "quickstart.md").read_text()
    llms_generator = (REPO_ROOT / "website" / "scripts" / "generate-llms-txt.py").read_text()

    assert ULTRA_INSTALL_URL in install
    assert ULTRA_INSTALL_URL in quickstart
    assert "hermes-ultra setup" in install
    assert "hermes-ultra model" in quickstart
    assert "INSTALL_LEGACY_ALIAS=true" in install
    assert "sheawinkler/hermes-agent-ultra" in llms_generator
