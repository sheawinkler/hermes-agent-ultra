"""Regression coverage for Termux in the Rust release installer."""

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
PYPROJECT = REPO_ROOT / "pyproject.toml"
INSTALL_SH = REPO_ROOT / "scripts" / "install.sh"


def test_rust_repo_does_not_require_python_termux_extra_profile() -> None:
    assert not PYPROJECT.exists()
    assert "termux-all" not in INSTALL_SH.read_text()


def test_install_script_uses_release_assets_for_termux_targets() -> None:
    text = INSTALL_SH.read_text()
    assert "detect_target()" in text
    assert 'Linux) os="linux" ;;' in text
    assert 'arm64|aarch64) arch="aarch64" ;;' in text
    assert 'ASSET_CANDIDATES=("${RELEASE_BIN_BASENAME}-${TARGET}.tar.gz")' in text
    assert '"${RELEASE_BIN_BASENAME}-linux-arm64.tar.gz"' in text
