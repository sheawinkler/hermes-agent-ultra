from __future__ import annotations

import base64
import datetime as dt
import hashlib
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-release-readiness-summary.py"
PARITY_AUDIT = REPO_ROOT / ".github" / "workflows" / "parity-audit.yml"
RUN_REPO_CI = REPO_ROOT / "scripts" / "run-repo-ci.sh"


def _load_module():
    spec = importlib.util.spec_from_file_location("release_readiness", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()


def _write_json(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

def _snapshot_local_source(
    repo: Path,
    *,
    reviewed_dirty_source: bool = False,
) -> dict[str, object]:
    queue_script = REPO_ROOT / "scripts" / "generate-upstream-patch-queue.py"
    spec = importlib.util.spec_from_file_location("queue_local_source", queue_script)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    local_source, errors = module.build_local_source_record(
        repo,
        reviewed_dirty_source=reviewed_dirty_source,
    )
    assert not errors, errors
    return local_source


def _fresh_fixture(repo: Path) -> tuple[dict[str, object], dict[str, object], dict[str, object]]:
    module = _load_module()
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "test@example.com")
    _git(repo, "config", "user.name", "Readiness Test")
    (repo / "tracked.txt").write_text("tracked\n", encoding="utf-8")
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "fixture")
    head = _git(repo, "rev-parse", "HEAD")
    now = dt.datetime.now(dt.timezone.utc).isoformat()
    range_digest = hashlib.sha256(head.encode("ascii")).hexdigest()
    classifications = [
        {
            "sha": head,
            "disposition": "ported",
            "classification_rule": "exact_sha_equivalence",
            "semantic_equivalence": {"method": "exact_sha", "fork_sha": head},
        }
    ]
    classification_digest = module.canonical_json_digest({"classifications": classifications})
    local_source = _snapshot_local_source(repo)
    ledger: dict[str, object] = {
        "schema_version": 2,
        "generator": {"name": "generate-upstream-patch-queue.py", "version": "3.0.0"},
        "generated_at_utc": now,
        "freshness": {
            "required_within_seconds": 3600,
            "state": "fresh",
            "freshly_fetched": True,
        },
        "sources": {
            name: {
                "canonical_url": f"https://example.invalid/{name}.git",
                "configured_canonical_url": f"https://example.invalid/{name}.git",
                "fetched_sha": head,
                "history": {"complete": True, "is_shallow_repository": False},
                "fetch": {"fetched_at_utc": now},
            }
            for name in ("fork", "upstream")
        },
        "merge_base": {"status": "present", "sha": head, "candidates": [head]},
        "range": {
            "base_ref": head,
            "commit_shas": [head],
            "commit_count": 1,
            "sha256": range_digest,
            "classifications": classifications,
            "classification_sha256": classification_digest,
            "complete": True,
        },
        "source_errors": [],
        "source_error_counts": {},
        "source_valid": True,
        "local_source": local_source,
        "local_source_sha256": module.canonical_json_digest(local_source),
    }
    ledger_digest = module.canonical_json_digest(ledger)
    queue_summary: dict[str, object] = {
        "total_commits": 1,
        "complete_range_commit_count": 1,
        "unowned_divergence_count": 0,
        "by_disposition": {"pending": 0, "ported": 1},
        "by_target_ticket": {"20": 1},
    }
    queue: dict[str, object] = {
        "schema_version": 2,
        "generated_at_utc": now,
        "source_ledger": {
            "path": "docs/parity/upstream-source-ledger.json",
            "sha256": ledger_digest,
            "fork_sha": head,
            "upstream_sha": head,
            "range_sha256": range_digest,
            "classification_sha256": classification_digest,
            "local_source_sha256": module.canonical_json_digest(local_source),
            "local_tree_state_digest": local_source["tree_state_digest"],
        },
        "summary": queue_summary,
        "truth": {"pass": True, "error_count": 0, "errors": []},
        "commits": classifications,
    }
    metrics = {metric: 0.0 for metric in module.REQUIRED_PROOF_METRICS}
    metrics["min_test_intent_mapping_ratio"] = 1.0
    metrics["min_test_coverage_tracked_behavior_ratio"] = 1.0
    metrics["min_fixture_reference_category_coverage_ratio"] = 1.0
    metrics["min_behavioral_similarity_ratio"] = 1.0
    metrics["min_deep_problem_solving_ratio"] = 1.0
    proof: dict[str, object] = {
        "generated_at_utc": now,
        "metrics": metrics,
        "queue_summary": queue_summary,
        "release_gate": {"pass": True},
        "truth": {
            "pass": True,
            "errors": [],
            "queue_provenance": {
                "pass": True,
                "errors": [],
                "ledger_sha256": ledger_digest,
                "local_source_sha256": module.canonical_json_digest(local_source),
                "local_tree_state_digest": local_source["tree_state_digest"],
            },
        },
    }
    _write_json(repo / "docs/parity/upstream-source-ledger.json", ledger)
    _write_json(repo / "docs/parity/upstream-missing-queue.json", queue)
    _write_json(repo / "docs/parity/global-parity-proof.json", proof)
    _write_json(repo / "docs/parity/shared-diff-backlog.json", {"summary": {"pending_classification": 0, "pending_review": 0}})
    _write_json(
        repo / "docs/parity/test-coverage-audit.json",
        {"summary": {"missing_rust_test_refs": 0}, "audit_gate": {"critical_gaps": 0}},
    )
    _write_json(
        repo / "docs/parity/sota-harness-matrix.json",
        {
            "authority": {
                "capability_promotion_authoritative": False,
                "production_complete": False,
            },
            "scope": {"coverage_kind": "fixture_reference_regression"},
            "summary": {},
            "gate": {
                "scope": "fixture_reference_regression",
                "pass": True,
                "critical_gaps": 0,
                "missing_rust_test_refs": 0,
            },
        },
    )
    return ledger, queue, proof


def _refresh_chain(
    repo: Path,
    ledger: dict[str, object],
    queue: dict[str, object],
    proof: dict[str, object],
    *,
    reviewed_dirty_source: bool = False,
) -> None:
    module = _load_module()
    local_source = _snapshot_local_source(
        repo,
        reviewed_dirty_source=reviewed_dirty_source,
    )
    local_source_digest = module.canonical_json_digest(local_source)
    ledger["local_source"] = local_source
    ledger["local_source_sha256"] = local_source_digest
    ledger_digest = module.canonical_json_digest(ledger)
    source = queue["source_ledger"]
    assert isinstance(source, dict)
    source["sha256"] = ledger_digest
    sources = ledger["sources"]
    assert isinstance(sources, dict)
    for name in ("fork", "upstream"):
        source[f"{name}_sha"] = sources[name]["fetched_sha"]
    commit_range = ledger["range"]
    assert isinstance(commit_range, dict)
    source["classification_sha256"] = commit_range["classification_sha256"]
    source["local_source_sha256"] = local_source_digest
    source["local_tree_state_digest"] = local_source["tree_state_digest"]
    proof["queue_summary"] = queue["summary"]
    proof_truth = proof["truth"]
    assert isinstance(proof_truth, dict)
    proof_truth["queue_provenance"] = {
        "pass": True,
        "errors": [],
        "ledger_sha256": ledger_digest,
        "local_source_sha256": local_source_digest,
        "local_tree_state_digest": local_source["tree_state_digest"],
    }
    _write_json(repo / "docs/parity/upstream-source-ledger.json", ledger)
    _write_json(repo / "docs/parity/upstream-missing-queue.json", queue)
    _write_json(repo / "docs/parity/global-parity-proof.json", proof)


def test_release_readiness_summary_accepts_complete_fresh_provenance(tmp_path: Path) -> None:
    _fresh_fixture(tmp_path)
    out_json = tmp_path / ".sync-reports/release-readiness-summary.json"
    out_md = tmp_path / ".sync-reports/release-readiness-summary.md"

    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(tmp_path),
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
    payload = json.loads(out_json.read_text(encoding="utf-8"))
    assert payload["ok"] is True
    assert payload["summary"]["source_integrity_pass"] is True
    assert payload["summary"]["queue_pending"] == 0
    assert "Status: **PASS**" in out_md.read_text(encoding="utf-8")


def test_readiness_reports_fixture_reference_hygiene_without_capability_promotion(
    tmp_path: Path,
) -> None:
    _fresh_fixture(tmp_path)
    matrix_path = tmp_path / "docs/parity/sota-harness-matrix.json"
    matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
    matrix["gate"]["pass"] = False
    matrix["gate"]["critical_gaps"] = 1
    _write_json(matrix_path, matrix)

    module = _load_module()
    summary = module.build_summary(tmp_path)

    assert summary["ok"] is True
    hygiene = summary["fixture_reference_hygiene"]
    assert hygiene["coverage_kind"] == "fixture_reference_regression"
    assert hygiene["capability_promotion_authoritative"] is False
    assert hygiene["production_complete"] is False
    assert hygiene["scope_valid"] is True
    assert hygiene["gate_pass"] is False
    assert hygiene["unresolved_gaps"] == 1
    assert not any(
        check["id"].startswith("fixture_reference") for check in summary["checks"]
    )
    markdown = module.render_markdown(summary)
    assert "Fixture-Reference Regression Hygiene (Non-Authoritative)" in markdown
    assert "Production complete: `false`" in markdown


def test_readiness_accepts_valid_reviewed_anchor_when_merge_base_is_absent(
    tmp_path: Path,
) -> None:
    ledger, queue, proof = _fresh_fixture(tmp_path)
    head = _git(tmp_path, "rev-parse", "HEAD")
    ledger["merge_base"] = {"status": "absent", "sha": None}
    ledger["reviewed_anchor"] = {
        "valid": True,
        "required": True,
        "anchor_sha": head,
        "review_basis": {"kind": "test", "semantic_evidence": "fixture evidence"},
        "evidence_fork_commit": head,
        "evidence_artifacts": [
            {
                "path": "docs/parity/anchor-evidence.json",
                "anchor_classification": {
                    "sha": head,
                    "disposition": "ported",
                    "classification_rule": "exact_sha_equivalence",
                },
            }
        ],
    }
    _refresh_chain(tmp_path, ledger, queue, proof)

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is True


def test_stale_source_sha_forces_readiness_failure(tmp_path: Path) -> None:
    ledger, queue, proof = _fresh_fixture(tmp_path)
    sources = ledger["sources"]
    assert isinstance(sources, dict)
    fork = sources["fork"]
    assert isinstance(fork, dict)
    fork["fetched_sha"] = "0" * 40
    _refresh_chain(tmp_path, ledger, queue, proof)

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is False
    assert any("local-source HEAD does not match" in issue for issue in summary["source_integrity"]["issues"])


def test_unclassified_delta_forces_readiness_failure(tmp_path: Path) -> None:
    ledger, queue, proof = _fresh_fixture(tmp_path)
    queue_summary = queue["summary"]
    assert isinstance(queue_summary, dict)
    queue_summary["unowned_divergence_count"] = 1
    queue_summary["by_disposition"] = {"pending": 1}
    queue["truth"] = {
        "pass": False,
        "error_count": 1,
        "errors": [{"code": "unowned_divergence"}],
    }
    ledger["source_valid"] = False
    ledger["source_errors"] = [{"code": "unowned_divergence"}]
    proof["truth"] = {
        "pass": False,
        "errors": ["source ledger reports source or classification errors"],
        "queue_provenance": {"pass": False, "errors": ["unowned divergence"]},
    }
    metrics = proof["metrics"]
    assert isinstance(metrics, dict)
    metrics["max_queue_pending_commits"] = 1.0
    _refresh_chain(tmp_path, ledger, queue, proof)

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is False
    checks = {check["id"]: check for check in summary["checks"]}
    assert checks["queue_pending"]["pass"] is False
    assert checks["queue_unowned"]["pass"] is False
    assert checks["source_integrity"]["pass"] is False


def test_missing_global_proof_metric_forces_readiness_failure(tmp_path: Path) -> None:
    ledger, queue, proof = _fresh_fixture(tmp_path)
    metrics = proof["metrics"]
    assert isinstance(metrics, dict)
    del metrics["max_queue_pending_commits"]
    _refresh_chain(tmp_path, ledger, queue, proof)

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is False
    assert any("max_queue_pending_commits is missing" in issue for issue in summary["source_integrity"]["issues"])


def test_dirty_local_source_forces_readiness_failure(tmp_path: Path) -> None:
    ledger, queue, proof = _fresh_fixture(tmp_path)
    (tmp_path / "tracked.txt").write_text("dirty\n", encoding="utf-8")
    _refresh_chain(
        tmp_path,
        ledger,
        queue,
        proof,
        reviewed_dirty_source=True,
    )

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is False
    assert any(
        "local_source is dirty or unavailable" in issue
        for issue in summary["source_integrity"]["issues"]
    )


def test_readiness_resample_fails_closed_without_exposing_private_untracked_data(
    tmp_path: Path,
) -> None:
    _fresh_fixture(tmp_path)
    private_path = Path("private") / "readiness-api-token.txt"
    private_bytes = b"not-for-readiness-summary\n"
    private_file = tmp_path / private_path
    private_file.parent.mkdir()
    private_file.write_bytes(private_bytes)

    summary = _load_module().build_summary(tmp_path)

    assert summary["ok"] is False
    assert any(
        "current local-source record does not match the canonical source ledger" in issue
        for issue in summary["source_integrity"]["issues"]
    )
    out_json = tmp_path / ".sync-reports/release-readiness-summary.json"
    out_md = tmp_path / ".sync-reports/release-readiness-summary.md"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(tmp_path),
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
    assert result.returncode == 1, result.stdout + result.stderr
    summary_text = (
        json.dumps(summary, sort_keys=True)
        + (tmp_path / "docs/parity/upstream-source-ledger.json").read_text(encoding="utf-8")
        + (tmp_path / "docs/parity/upstream-missing-queue.json").read_text(encoding="utf-8")
        + (tmp_path / "docs/parity/global-parity-proof.json").read_text(encoding="utf-8")
        + out_json.read_text(encoding="utf-8")
        + out_md.read_text(encoding="utf-8")
    )
    for private_value in (
        private_path.as_posix(),
        base64.b64encode(private_path.as_posix().encode("utf-8")).decode("ascii"),
        private_bytes.decode("utf-8").strip(),
        hashlib.sha256(private_bytes).hexdigest(),
        str(tmp_path.resolve()),
        tmp_path.resolve().as_uri(),
    ):
        assert private_value not in summary_text


def test_release_readiness_summary_uses_public_artifacts_only() -> None:
    text = SCRIPT.read_text(encoding="utf-8")
    assert "docs/parity/global-parity-proof.json" in text
    assert "docs/parity/upstream-missing-queue.json" in text
    assert "docs/parity/upstream-source-ledger.json" in text
    assert "docs/parity/shared-diff-backlog.json" in text
    assert "docs/parity/test-coverage-audit.json" in text
    assert "docs/parity/sota-harness-matrix.json" in text
    assert "contextlattice" not in text.lower()


def test_repo_ci_runs_readiness_summary_after_global_parity_proof() -> None:
    text = RUN_REPO_CI.read_text(encoding="utf-8")
    assert "python3 scripts/generate-global-parity-proof.py" in text
    assert "python3 scripts/generate-release-readiness-summary.py --check" in text
    assert text.index("python3 scripts/generate-global-parity-proof.py") < text.index(
        "python3 scripts/generate-release-readiness-summary.py --check"
    )


def test_parity_audit_emits_readiness_summary_before_enforcing_ci_gate() -> None:
    text = PARITY_AUDIT.read_text(encoding="utf-8")
    assert "id: parity-proof" in text
    assert "python3 scripts/generate-global-parity-proof.py --check-ci || status=$?" in text
    assert "Generate public release readiness summary" in text
    assert "Enforce global parity CI gate" in text
    assert text.index("Generate public release readiness summary") < text.index(
        "Enforce global parity CI gate"
    )
    assert "raise SystemExit(0)" not in text
    assert 'ci_gate = raw.get("ci_gate", {})' in text
