from pathlib import Path
import re
import tomllib


REPO_ROOT = Path(__file__).resolve().parents[1]


def test_homebrew_formula_tracks_workspace_release_version():
    cargo = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = cargo["workspace"]["package"]["version"]
    formula = (REPO_ROOT / "packaging/homebrew/hermes-agent.rb").read_text(
        encoding="utf-8"
    )

    assert f'version "{version}"' in formula
    assert f"/releases/download/v{version}/" in formula
    assert "REPLACE_WITH_ACTUAL_SHA256" not in formula
    assert "v0.1.0" not in formula


def test_homebrew_formula_has_platform_specific_checksummed_assets():
    formula = (REPO_ROOT / "packaging/homebrew/hermes-agent.rb").read_text(
        encoding="utf-8"
    )

    for asset in [
        "hermes-macos-aarch64.tar.gz",
        "hermes-macos-x86_64.tar.gz",
        "hermes-linux-aarch64.tar.gz",
        "hermes-linux-x86_64.tar.gz",
    ]:
        assert asset in formula

    shas = re.findall(r'sha256 "([0-9a-f]{64})"', formula)
    assert len(shas) == 4


def test_homebrew_formula_does_not_clobber_upstream_hermes_command():
    formula = (REPO_ROOT / "packaging/homebrew/hermes-agent.rb").read_text(
        encoding="utf-8"
    )

    assert 'bin.install "hermes" => "hermes-agent-ultra"' in formula
    assert 'bin.install_symlink "hermes-agent-ultra" => "hermes-ultra"' in formula
    assert '=> "hermes"' not in formula
