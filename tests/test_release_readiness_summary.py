import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-release-readiness-summary.py"
PARITY_AUDIT = REPO_ROOT / ".github" / "workflows" / "parity-audit.yml"


def test_release_readiness_summary_reports_zero_backlog(tmp_path: Path) -> None:
    out_json = tmp_path / "summary.json"
    out_md = tmp_path / "summary.md"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(REPO_ROOT),
            "--output-json",
            str(out_json),
            "--output-md",
            str(out_md),
            "--check",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    payload = json.loads(out_json.read_text())
    assert payload["ok"] is True
    assert payload["summary"]["queue_pending"] == 0
    assert payload["summary"]["shared_pending_classification"] == 0
    assert payload["summary"]["shared_pending_review"] == 0
    assert payload["summary"]["coverage_critical_gaps"] == 0
    assert payload["summary"]["sota_critical_gaps"] == 0
    text = out_md.read_text()
    assert "Status: **PASS**" in text
    assert "Upstream queue pending" in text


def test_release_readiness_summary_uses_public_artifacts_only() -> None:
    text = SCRIPT.read_text()
    assert "docs/parity/global-parity-proof.json" in text
    assert "docs/parity/upstream-missing-queue.json" in text
    assert "docs/parity/shared-diff-backlog.json" in text
    assert "docs/parity/test-coverage-audit.json" in text
    assert "docs/parity/sota-harness-matrix.json" in text
    assert "contextlattice" not in text.lower()


def test_parity_audit_emits_readiness_summary_before_enforcing_ci_gate() -> None:
    text = PARITY_AUDIT.read_text()
    assert "id: parity-proof" in text
    assert "python3 scripts/generate-global-parity-proof.py --check-ci || status=$?" in text
    assert "Generate public release readiness summary" in text
    assert "Enforce global parity CI gate" in text
    assert text.index("Generate public release readiness summary") < text.index(
        "Enforce global parity CI gate"
    )
    assert "raise SystemExit(0)" not in text
    assert "ci_gate = raw.get(\"ci_gate\", {})" in text
