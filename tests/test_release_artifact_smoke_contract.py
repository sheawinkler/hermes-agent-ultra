from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "smoke-release-artifact.sh"
CARGO = REPO_ROOT / "Cargo.toml"
RELEASE_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"


def _workspace_version() -> str:
    for line in CARGO.read_text().splitlines():
        if line.startswith("version = "):
            return line.split('"', 2)[1]
    raise AssertionError("workspace version not found")


def test_release_smoke_defaults_to_current_workspace_tag() -> None:
    text = SCRIPT.read_text()
    assert f'VERSION="${{VERSION:-v{_workspace_version()}}}"' in text


def test_release_smoke_downloads_release_installer_not_local_script() -> None:
    text = SCRIPT.read_text()
    assert 'https://github.com/${REPO}/releases/download/${VERSION}/install.sh' in text
    assert 'curl -fsSL "${INSTALLER_URL}" -o "${INSTALLER}"' in text
    assert '"${INSTALLER}" "${VERSION}" --dir "${BIN_DIR}"' in text


def test_release_smoke_fails_source_fallback_and_preserves_upstream_hermes() -> None:
    text = SCRIPT.read_text()
    assert 'INSTALL_LEGACY_ALIAS=false' in text
    assert 'upstream-hermes-sentinel' in text
    assert 'Installer clobbered existing hermes command' in text
    assert 'assert_not_contains "${INSTALL_LOG}" "building from source"' in text
    assert 'assert_not_contains "${INSTALL_LOG}" "Release asset not available"' in text


def test_release_smoke_is_portable_across_macos_and_linux_hash_tools() -> None:
    text = SCRIPT.read_text()
    assert "sha256_file()" in text
    assert "command -v shasum" in text
    assert "command -v sha256sum" in text
    assert "Missing required command: shasum or sha256sum" in text


def test_release_smoke_bounds_each_runtime_probe() -> None:
    text = SCRIPT.read_text()
    assert 'HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS="${HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS:-30}"' in text
    assert "Release smoke probe '${label}' timed out." in text
    assert "Command timed out after ${HERMES_SMOKE_COMMAND_TIMEOUT_SECONDS}s: $*" in text


def test_release_smoke_covers_public_runtime_surfaces() -> None:
    text = SCRIPT.read_text()
    for surface in [
        'setup-help',
        'auth-status',
        'memory-status',
        'route-health',
        'systems-status',
        'systems-release',
        'tools-list',
        'harness_cockpit',
        'tool_policy_simulate',
        'skills_list',
    ]:
        assert surface in text


def test_release_workflow_packages_canonical_binary_not_legacy_wrapper() -> None:
    text = RELEASE_WORKFLOW.read_text()
    assert "--bin hermes-agent-ultra" in text
    assert "binary: hermes-agent-ultra" in text
    assert "binary: hermes-agent-ultra.exe" in text
    assert "packaged: hermes" in text
    assert "packaged: hermes.exe" in text
    assert "release/hermes-agent-ultra dist/hermes" in text
    assert "binary: hermes\n" not in text
    assert "release/hermes dist/" not in text
