from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "run-repo-ci.sh"
README = REPO_ROOT / "README.md"
CI = REPO_ROOT / ".github" / "workflows" / "ci.yml"


def test_repo_ci_runner_exists_and_declares_authority() -> None:
    text = SCRIPT.read_text()
    assert SCRIPT.exists()
    assert "repository-local CI contract" in text
    assert "hosted GitHub Actions are a convenience mirror" in text
    assert "refs/heads/main:refs/remotes/${UPSTREAM_REMOTE}/main" in text
    assert "[repo-ci] PASS" in text


def test_repo_ci_runner_covers_workflow_test_job_gates() -> None:
    script = SCRIPT.read_text()
    workflow = CI.read_text()
    for command in [
        "python3 scripts/generate-parity-matrix.py --local-ref HEAD",
        "python3 scripts/generate-workstream-status.py",
        "python3 scripts/generate-test-intent-mapping.py",
        "python3 scripts/generate-test-coverage-audit.py --check",
        "python3 scripts/generate-adapter-matrix.py",
        "python3 scripts/validate-intentional-divergence.py --check --allow-warnings",
        "python3 scripts/generate-shared-diff-backlog.py --local-ref HEAD --no-fetch",
        "python3 scripts/generate-upstream-patch-queue.py --local-ref HEAD --max-commits 0",
        "python3 scripts/generate-global-parity-proof.py",
        "cargo fmt --all --check",
        "bash scripts/clippy-warning-gate.sh --check",
        "cargo test -p hermes-source-parity-tests --test rust_module_size_policy -- --nocapture",
        "bash scripts/check-runtime-placeholders.sh",
        "cargo test --workspace",
        "cargo test -p hermes-parity-tests",
        "cargo test -p hermes-source-parity-tests --test cli_command_contract",
        "cargo test -p hermes-protocol-parity-tests --test protocol_differential_contracts",
        "cargo test -p hermes-source-parity-tests --test global_parity_governance",
    ]:
        assert command in workflow
        assert command in script

    assert 'python3 scripts/run-upstream-slash-parity-gate.py --upstream-ref "${UPSTREAM_REF}"' in script
    assert 'python3 scripts/run-upstream-surface-coverage-gate.py --repo-root . --upstream-ref "${UPSTREAM_REF}" --local-ref HEAD' in script


def test_readme_names_repo_ci_as_gold_standard() -> None:
    text = README.read_text()
    assert "Repo-local CI is the authoritative gate" in text
    assert "GitHub Actions are a hosted mirror" in text
    assert "bash scripts/run-repo-ci.sh" in text
