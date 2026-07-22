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
SCRIPT = REPO_ROOT / "scripts" / "generate-global-parity-proof.py"


def _write(path: Path, payload: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _digest(payload: dict[str, object]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()

def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=cwd,
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()


def _snapshot_local_source(root: Path, *, reviewed_dirty_source: bool = False) -> dict[str, object]:
    spec = importlib.util.spec_from_file_location("queue_local_source", REPO_ROOT / "scripts" / "generate-upstream-patch-queue.py")
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    local_source, errors = module.build_local_source_record(
        root,
        reviewed_dirty_source=reviewed_dirty_source,
    )
    assert not errors, errors
    return local_source


def _sync_local_source(
    root: Path,
    ledger: dict[str, object],
    queue: dict[str, object],
    *,
    reviewed_dirty_source: bool = False,
) -> None:
    local_source = _snapshot_local_source(root, reviewed_dirty_source=reviewed_dirty_source)
    local_source_digest = _digest(local_source)
    ledger["local_source"] = local_source
    ledger["local_source_sha256"] = local_source_digest
    source = queue["source_ledger"]
    assert isinstance(source, dict)
    source["local_source_sha256"] = local_source_digest
    source["local_tree_state_digest"] = local_source["tree_state_digest"]
    source["sha256"] = _digest(ledger)


def _proof_fixture(root: Path) -> tuple[dict[str, object], dict[str, object]]:
    _git(root, "init", "-q")
    _git(root, "config", "user.email", "test@example.com")
    _git(root, "config", "user.name", "Parity Proof Test")
    docs = root / "docs/parity"
    _write(
        docs / "global-parity-thresholds.json",
        {
            "ci_thresholds": {"metric_thresholds": {}},
            "release_thresholds": {
                "metric_thresholds": {},
                "required_workstreams_complete": [f"GPAR-{number:02d}" for number in range(1, 12)],
            },
        },
    )
    (root / "source.txt").write_text("source\n", encoding="utf-8")
    _git(root, "add", ".")
    _git(root, "commit", "-qm", "fixture source")
    now = dt.datetime.now(dt.timezone.utc).isoformat()
    sha = _git(root, "rev-parse", "HEAD")
    range_digest = hashlib.sha256(sha.encode("ascii")).hexdigest()
    classifications = [
        {
            "sha": sha,
            "disposition": "ported",
            "classification_rule": "exact_sha_equivalence",
            "semantic_equivalence": {"method": "exact_sha", "fork_sha": sha},
        }
    ]
    classification_digest = _digest({"classifications": classifications})
    local_source = _snapshot_local_source(root)
    ledger: dict[str, object] = {
        "schema_version": 2,
        "generator": {"name": "generate-upstream-patch-queue.py", "version": "3.0.0"},
        "freshness": {
            "required_within_seconds": 3600,
            "state": "fresh",
            "freshly_fetched": True,
        },
        "source_valid": True,
        "source_errors": [],
        "local_source": local_source,
        "local_source_sha256": _digest(local_source),
        "sources": {
            name: {
                "fetched_sha": sha,
                "canonical_url": f"https://example.invalid/{name}.git",
                "configured_canonical_url": f"https://example.invalid/{name}.git",
                "history": {"complete": True, "is_shallow_repository": False},
                "fetch": {"fetched_at_utc": now},
            }
            for name in ("fork", "upstream")
        },
        "merge_base": {"status": "present", "sha": sha},
        "range": {
            "base_ref": sha,
            "commit_shas": [sha],
            "commit_count": 1,
            "sha256": range_digest,
            "classifications": classifications,
            "classification_sha256": classification_digest,
            "complete": True,
        },
    }
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
            "sha256": _digest(ledger),
            "fork_sha": sha,
            "upstream_sha": sha,
            "range_sha256": range_digest,
            "classification_sha256": classification_digest,
            "local_source_sha256": _digest(local_source),
            "local_tree_state_digest": local_source["tree_state_digest"],
        },
        "summary": queue_summary,
        "truth": {"pass": True, "error_count": 0, "errors": []},
        "commits": classifications,
    }
    _write(docs / "upstream-source-ledger.json", ledger)
    _write(docs / "upstream-missing-queue.json", queue)
    _write(
        docs / "global-parity-thresholds.json",
        {
            "ci_thresholds": {"metric_thresholds": {}},
            "release_thresholds": {
                "metric_thresholds": {},
                "required_workstreams_complete": [f"GPAR-{number:02d}" for number in range(1, 12)],
            },
        },
    )
    _write(
        docs / "parity-matrix.json",
        {
            "refs": {"local_sha": sha, "upstream_sha": sha},
            "summary": {"commits_behind": 0, "upstream_patch_missing": 0, "files_only_upstream": 0},
            "intentional_divergence": [{"id": "rust-cli-tui-primary-ux-surface"}],
            "top_shared_different": [],
        },
    )
    _write(docs / "workstream-status.json", {"states": {}})
    _write(
        docs / "test-intent-mapping.json",
        {
            "summary": {"mapping_ratio": 1.0},
            "intents": [
                {"id": "skills-management-contract", "mapped": True},
                {"id": "environment-lifecycle-contract", "mapped": True},
                {"id": "tool-call-parser-contract", "mapped": True},
            ],
        },
    )
    _write(docs / "adapter-feature-matrix.json", {"summary": {"non_rust_native": 0}})
    _write(docs / "shared-different-classification.json", {"items": []})
    _write(docs / "divergence-validation.json", {"summary": {"errors": 0, "unowned": 0, "review_overdue": 0}})
    _write(
        docs / "test-coverage-audit.json",
        {
            "summary": {"tracked_behavior_coverage_ratio": 1.0, "missing_rust_test_refs": 0},
            "audit_gate": {"pass": True, "critical_gaps": 0},
        },
    )
    _write(
        docs / "sota-harness-matrix.json",
        {
            "authority": {
                "capability_promotion_authoritative": False,
                "production_complete": False,
            },
            "scope": {"coverage_kind": "fixture_reference_regression"},
            "summary": {"domain_coverage_ratio": 1.0},
            "gate": {
                "scope": "fixture_reference_regression",
                "pass": True,
                "critical_gaps": 0,
                "missing_rust_test_refs": 0,
            },
        },
    )
    _write(
        docs / "behavioral-similarity-diff.json",
        {
            "summary": {
                "behavioral_similarity_ratio": 1.0,
                "superior_cases": 1,
                "regressions": 0,
                "gaps": 0,
                "unverified_cases": 0,
                "missing_rust_test_refs": 0,
            },
            "gate": {"pass": True},
        },
    )
    _write(
        docs / "deep-problem-solving-diff.json",
        {
            "summary": {
                "deep_problem_solving_ratio": 1.0,
                "superior_cases": 1,
                "total_cases": 1,
                "regressions": 0,
                "gaps": 0,
                "unverified_cases": 0,
                "missing_rust_test_refs": 0,
            },
            "gate": {"pass": True},
        },
    )
    return ledger, queue


def _run(root: Path) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    output = root / "docs/parity/global-parity-proof.json"
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPT),
            "--repo-root",
            str(root),
            "--out-json",
            str(output),
            "--out-md",
            str(root / "docs/parity/global-parity-proof.md"),
            "--check-release",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    return result, json.loads(output.read_text(encoding="utf-8"))


def test_global_proof_fails_and_rewrites_when_queue_ledger_has_source_errors(tmp_path: Path) -> None:
    ledger, _ = _proof_fixture(tmp_path)
    ledger["source_valid"] = False
    ledger["source_errors"] = [{"code": "missing_merge_base"}]
    _write(tmp_path / "docs/parity/upstream-source-ledger.json", ledger)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert proof["release_gate"]["pass"] is False
    assert any(
        check["metric"] == "source_provenance_and_required_metrics"
        for check in proof["release_gate"]["checks"]
    )


def test_global_proof_fails_when_required_source_metric_is_missing(tmp_path: Path) -> None:
    _proof_fixture(tmp_path)
    parity_path = tmp_path / "docs/parity/parity-matrix.json"
    parity = json.loads(parity_path.read_text(encoding="utf-8"))
    del parity["summary"]["upstream_patch_missing"]
    _write(parity_path, parity)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert proof["truth"]["missing_metric_count"] >= 1
    assert proof["release_gate"]["pass"] is False


def test_global_proof_fails_when_parity_matrix_sha_is_stale(tmp_path: Path) -> None:
    _proof_fixture(tmp_path)
    parity_path = tmp_path / "docs/parity/parity-matrix.json"
    parity = json.loads(parity_path.read_text(encoding="utf-8"))
    parity["refs"]["upstream_sha"] = "b" * 40
    _write(parity_path, parity)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert any("upstream SHA is stale" in error for error in proof["truth"]["errors"])


def test_global_proof_fails_when_source_fetch_timestamp_is_stale(tmp_path: Path) -> None:
    ledger, queue = _proof_fixture(tmp_path)
    sources = ledger["sources"]
    assert isinstance(sources, dict)
    for source in sources.values():
        assert isinstance(source, dict)
        source["fetch"] = {"fetched_at_utc": "2000-01-01T00:00:00+00:00"}
    source = queue["source_ledger"]
    assert isinstance(source, dict)
    source["sha256"] = _digest(ledger)
    _write(tmp_path / "docs/parity/upstream-source-ledger.json", ledger)
    _write(tmp_path / "docs/parity/upstream-missing-queue.json", queue)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert any("fetch timestamp is stale" in error for error in proof["truth"]["errors"])


def test_global_proof_accepts_valid_reviewed_anchor_when_merge_base_is_absent(
    tmp_path: Path,
) -> None:
    ledger, queue = _proof_fixture(tmp_path)
    sha = _git(tmp_path, "rev-parse", "HEAD")
    ledger["merge_base"] = {"status": "absent", "sha": None}
    ledger["reviewed_anchor"] = {
        "valid": True,
        "required": True,
        "anchor_sha": sha,
        "review_basis": {"kind": "test", "semantic_evidence": "fixture evidence"},
        "evidence_fork_commit": sha,
        "evidence_artifacts": [
            {
                "path": "docs/parity/anchor-evidence.json",
                "anchor_classification": {
                    "sha": sha,
                    "disposition": "ported",
                    "classification_rule": "exact_sha_equivalence",
                },
            }
        ],
    }
    source = queue["source_ledger"]
    assert isinstance(source, dict)
    source["sha256"] = _digest(ledger)
    _write(tmp_path / "docs/parity/upstream-source-ledger.json", ledger)
    _write(tmp_path / "docs/parity/upstream-missing-queue.json", queue)

    result, proof = _run(tmp_path)

    assert result.returncode == 0, result.stdout + result.stderr
    assert proof["truth"]["pass"] is True


def test_global_proof_serializes_fixture_reference_scope_without_product_promotion(
    tmp_path: Path,
) -> None:
    _proof_fixture(tmp_path)

    result, proof = _run(tmp_path)

    assert result.returncode == 0, result.stdout + result.stderr
    assert proof["fixture_reference_coverage"]["authority"] == {
        "capability_promotion_authoritative": False,
        "production_complete": False,
    }
    assert proof["fixture_reference_coverage"]["scope"] == {
        "coverage_kind": "fixture_reference_regression"
    }
    markdown = (tmp_path / "docs/parity/global-parity-proof.md").read_text(encoding="utf-8")
    assert "Fixture-Reference Regression Hygiene (Non-Authoritative)" in markdown
    assert "Production complete: `false`" in markdown


def test_global_proof_rejects_fixture_reference_production_claim(tmp_path: Path) -> None:
    _proof_fixture(tmp_path)
    matrix_path = tmp_path / "docs/parity/sota-harness-matrix.json"
    matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
    matrix["authority"]["production_complete"] = True
    _write(matrix_path, matrix)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert any(
        "production_complete must be false" in error for error in proof["truth"]["errors"]
    )


def test_global_proof_fails_without_merge_base_or_valid_reviewed_anchor(
    tmp_path: Path,
) -> None:
    ledger, queue = _proof_fixture(tmp_path)
    ledger["merge_base"] = {"status": "absent", "sha": None}
    source = queue["source_ledger"]
    assert isinstance(source, dict)
    source["sha256"] = _digest(ledger)
    _write(tmp_path / "docs/parity/upstream-source-ledger.json", ledger)
    _write(tmp_path / "docs/parity/upstream-missing-queue.json", queue)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["truth"]["pass"] is False
    assert any(
        "neither an exact merge base nor a validated reviewed anchor" in error
        for error in proof["truth"]["errors"]
    )


def test_global_proof_fails_closed_for_dirty_local_source(tmp_path: Path) -> None:
    ledger, queue = _proof_fixture(tmp_path)
    (tmp_path / "source.txt").write_text("dirty source\n", encoding="utf-8")
    _sync_local_source(
        tmp_path,
        ledger,
        queue,
        reviewed_dirty_source=True,
    )
    _write(tmp_path / "docs/parity/upstream-source-ledger.json", ledger)
    _write(tmp_path / "docs/parity/upstream-missing-queue.json", queue)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert proof["release_gate"]["pass"] is False
    assert any(
        "local_source is dirty or unavailable" in error for error in proof["truth"]["errors"]
    )


def test_global_proof_resample_fails_closed_without_exposing_private_untracked_data(
    tmp_path: Path,
) -> None:
    _proof_fixture(tmp_path)
    private_path = Path("private") / "proof-api-token.txt"
    private_bytes = b"not-for-global-proof\n"
    private_file = tmp_path / private_path
    private_file.parent.mkdir()
    private_file.write_bytes(private_bytes)

    result, proof = _run(tmp_path)

    assert result.returncode == 1
    assert any(
        "current local-source record does not match the canonical source ledger"
        in error
        for error in proof["truth"]["errors"]
    )
    queue_provenance = proof["truth"]["queue_provenance"]
    assert isinstance(queue_provenance, dict)
    assert queue_provenance["ledger_path"] == "docs/parity/upstream-source-ledger.json"
    proof_text = (
        json.dumps(proof, sort_keys=True)
        + (tmp_path / "docs/parity/global-parity-proof.md").read_text(encoding="utf-8")
    )
    for private_value in (
        private_path.as_posix(),
        base64.b64encode(private_path.as_posix().encode("utf-8")).decode("ascii"),
        private_bytes.decode("utf-8").strip(),
        hashlib.sha256(private_bytes).hexdigest(),
        str(tmp_path.resolve()),
        tmp_path.resolve().as_uri(),
    ):
        assert private_value not in proof_text
