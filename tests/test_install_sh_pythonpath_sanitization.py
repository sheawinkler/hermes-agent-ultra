"""Contracts for the Rust release installer environment behavior.

Hermes Agent Ultra installs a release binary, not an editable Python checkout.
The only Python-environment interaction left is post-install binary probing,
which must run with inherited Python vars cleared so host tooling cannot shadow
runtime diagnostics.
"""

from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
INSTALL_SH = REPO_ROOT / "scripts" / "install.sh"


def test_post_install_flow_clears_python_environment_for_binary_probes() -> None:
    text = INSTALL_SH.read_text()

    for var in (
        "PYTHONHOME",
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "VIRTUAL_ENV",
        "CONDA_PREFIX",
        "CONDA_DEFAULT_ENV",
        "PIP_REQUIRE_VIRTUALENV",
        "PYTHONUSERBASE",
    ):
        assert f"-u {var}" in text


def test_post_install_flow_wraps_binary_probes_with_timeout() -> None:
    text = INSTALL_SH.read_text()

    assert "HERMES_INSTALL_PROBE_TIMEOUT_SECONDS" in text
    assert "run_bounded_post_install_cmd()" in text
    assert (
        'run_bounded_post_install_cmd "doctor" "${clean_python_env[@]}" "${bin_path}" doctor || true'
        in text
    )
    assert (
        'run_bounded_post_install_cmd "auth status" "${clean_python_env[@]}" "${bin_path}" auth status || true'
        in text
    )
    assert (
        'run_bounded_post_install_cmd "setup" "${clean_python_env[@]}" "${bin_path}" setup || true'
        in text
    )


def test_installer_uses_binary_symlinks_not_python_launcher_wrapper() -> None:
    text = INSTALL_SH.read_text()

    assert 'install -m 0755 "${SOURCE_BIN}" "${INSTALL_DIR}/${CANONICAL_BIN_NAME}"' in text
    assert 'ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${PRIMARY_BIN_NAME}"' in text
    assert 'INSTALL_LEGACY_ALIAS="${INSTALL_LEGACY_ALIAS:-false}"' in text
    assert 'truthy_env "${INSTALL_LEGACY_ALIAS}"' in text
    assert 'ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${LEGACY_BIN_NAME}"' in text
    assert 'cat > "$command_link_dir/hermes" <<EOF' not in text
    assert "pip install" not in text
    assert "uv pip install" not in text


def test_installer_keeps_legacy_hermes_alias_opt_in() -> None:
    text = INSTALL_SH.read_text()

    assert 'INSTALL_LEGACY_ALIAS="${INSTALL_LEGACY_ALIAS:-false}"' in text
    legacy_link = 'ln -sfn "${CANONICAL_BIN_NAME}" "${INSTALL_DIR}/${LEGACY_BIN_NAME}"'
    assert text.index('truthy_env "${INSTALL_LEGACY_ALIAS}"') < text.index(legacy_link)
