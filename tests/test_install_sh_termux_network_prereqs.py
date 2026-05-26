"""Termux contracts for the Rust release installer."""

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = REPO_ROOT / "scripts" / "install.sh"


def test_termux_installs_into_prefix_bin() -> None:
    text = INSTALL_SH.read_text()

    assert "is_termux()" in text
    assert '[[ -n "${TERMUX_VERSION:-}" ]]' in text
    assert '[[ "${PREFIX:-}" == *"com.termux/files/usr"* ]]' in text
    assert 'INSTALL_DIR="${PREFIX}/bin"' in text


def test_installer_requires_release_download_tools_only() -> None:
    text = INSTALL_SH.read_text()

    assert "need_cmd curl" in text
    assert "need_cmd tar" in text
    assert "need_cmd install" in text
    assert "pkg install" not in text
    assert "pip install" not in text
