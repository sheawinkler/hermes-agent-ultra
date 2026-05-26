from pathlib import Path
import tomllib


REPO_ROOT = Path(__file__).resolve().parents[1]


def test_flake_tracks_workspace_release_version():
    cargo = tomllib.loads((REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = cargo["workspace"]["package"]["version"]
    flake = (REPO_ROOT / "flake.nix").read_text(encoding="utf-8")

    assert f'version = "{version}";' in flake
    assert 'version = "0.1.0";' not in flake
