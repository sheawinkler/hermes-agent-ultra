#!/usr/bin/env python3
"""Generate a fail-closed public release/parity readiness summary."""

from __future__ import annotations

import argparse
import importlib.util
import datetime as dt
import hashlib
import json
import math
from functools import lru_cache
import re
import subprocess
from pathlib import Path
from typing import Any

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
REQUIRED_PROOF_METRICS = (
    "max_queue_pending_commits",
    "max_unowned_divergences",
    "max_files_only_upstream",
    "min_test_intent_mapping_ratio",
    "min_test_coverage_tracked_behavior_ratio",
    "min_fixture_reference_category_coverage_ratio",
    "min_behavioral_similarity_ratio",
    "min_deep_problem_solving_ratio",
)


def canonical_json_digest(payload: dict[str, Any]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def is_full_sha(value: object) -> bool:
    return bool(SHA_RE.fullmatch(str(value).strip().lower()))

@lru_cache(maxsize=1)
def _local_source_contract_module() -> Any:
    path = Path(__file__).with_name("generate-upstream-patch-queue.py")
    spec = importlib.util.spec_from_file_location("upstream_patch_queue_local_source", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("canonical local-source contract is unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def validate_local_source_record(local_source: object) -> list[str]:
    return list(_local_source_contract_module().validate_local_source_record(local_source))


def sanitize_public_message(message: object) -> str:
    return str(_local_source_contract_module().sanitize_public_message(message))


def resample_local_source(repo_root: Path) -> tuple[dict[str, Any] | None, list[str]]:
    try:
        record, errors = _local_source_contract_module().build_local_source_record(
            repo_root,
            reviewed_dirty_source=False,
        )
    except (OSError, RuntimeError, AttributeError) as exc:
        return None, [
            sanitize_public_message(f"canonical local-source resample failed: {exc}")
        ]
    return record, [sanitize_public_message(error) for error in errors]

def generated_evidence_output_error(repo_root: Path, path: Path) -> str | None:
    return _local_source_contract_module().generated_evidence_output_error(repo_root, path)


def load_json(path: Path, label: str, issues: list[str]) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        issues.append(sanitize_public_message(f"{label} is unreadable: {exc}"))
        return {}
    if not isinstance(payload, dict):
        issues.append(f"{label} root must be an object")
        return {}
    return payload


def get_path(payload: dict[str, Any], *keys: str) -> Any:
    current: Any = payload
    for key in keys:
        if not isinstance(current, dict) or key not in current:
            return None
        current = current[key]
    return current


def as_finite_number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def as_nonnegative_int(value: Any) -> int | None:
    number = as_finite_number(value)
    if number is None or number < 0 or not number.is_integer():
        return None
    return int(number)


def parse_utc(value: Any) -> dt.datetime | None:
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(dt.timezone.utc)


def git_output(repo_root: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=repo_root,
        text=True,
        capture_output=True,
        check=False,
    )
    return proc.stdout.strip() if proc.returncode == 0 else ""


def workspace_version(repo_root: Path) -> str:
    cargo = repo_root / "Cargo.toml"
    text = cargo.read_text(encoding="utf-8") if cargo.exists() else ""
    match = re.search(r'(?m)^version = "([^"]+)"', text)
    return match.group(1) if match else "unknown"


def validate_source_integrity(
    repo_root: Path,
    proof: dict[str, Any],
    queue: dict[str, Any],
    ledger: dict[str, Any],
    max_source_age_seconds: int,
) -> dict[str, Any]:
    """Validate the only authoritative queue source chain without defaults."""

    issues: list[str] = []
    if max_source_age_seconds <= 0:
        issues.append("configured source freshness bound must be positive")
    if proof.get("truth", {}).get("pass") is not True:
        issues.append("global proof truth status is not PASS")
    proof_truth_errors = get_path(proof, "truth", "errors")
    if proof_truth_errors not in ([], None):
        issues.append("global proof reports source or required-metric errors")
    proof_queue_provenance = get_path(proof, "truth", "queue_provenance")
    if not isinstance(proof_queue_provenance, dict) or proof_queue_provenance.get("pass") is not True:
        issues.append("global proof lacks passing queue provenance")

    metrics = proof.get("metrics")
    if not isinstance(metrics, dict):
        issues.append("global proof metrics is missing or malformed")
    else:
        for metric_name in REQUIRED_PROOF_METRICS:
            if as_finite_number(metrics.get(metric_name)) is None:
                issues.append(f"global proof required metric {metric_name} is missing or malformed")

    if queue.get("schema_version") != 2:
        issues.append("patch queue schema_version must be 2")
    queue_truth = queue.get("truth")
    if not isinstance(queue_truth, dict) or queue_truth.get("pass") is not True:
        issues.append("patch queue truth status is not PASS")
    elif queue_truth.get("error_count") != 0 or queue_truth.get("errors") not in ([], None):
        issues.append("patch queue reports source or classification errors")
    queue_summary = queue.get("summary")
    if not isinstance(queue_summary, dict):
        issues.append("patch queue summary is missing or malformed")
        queue_summary = {}
    for key in ("total_commits", "complete_range_commit_count", "unowned_divergence_count"):
        if as_nonnegative_int(queue_summary.get(key)) is None:
            issues.append(f"patch queue summary.{key} is missing or malformed")
    by_disposition = queue_summary.get("by_disposition")
    if not isinstance(by_disposition, dict) or as_nonnegative_int(by_disposition.get("pending")) is None:
        issues.append("patch queue summary.by_disposition.pending is missing or malformed")

    source_ref = queue.get("source_ledger")
    if not isinstance(source_ref, dict):
        issues.append("patch queue source_ledger is missing or malformed")
        source_ref = {}
    if source_ref.get("path") != "docs/parity/upstream-source-ledger.json":
        issues.append("patch queue does not reference the canonical source ledger")
    if source_ref.get("sha256") != canonical_json_digest(ledger):
        issues.append("patch queue source-ledger digest does not match canonical ledger")

    if ledger.get("schema_version") != 2:
        issues.append("source ledger schema_version must be 2")
    generator = ledger.get("generator")
    if not isinstance(generator, dict) or not str(generator.get("version", "")).strip():
        issues.append("source ledger generator provenance is missing")
    if ledger.get("source_valid") is not True:
        issues.append("source ledger is not valid")
    source_errors = ledger.get("source_errors")
    if not isinstance(source_errors, list) or source_errors:
        issues.append("source ledger reports source or classification errors")

    freshness = ledger.get("freshness")
    if not isinstance(freshness, dict) or freshness.get("state") != "fresh" or freshness.get(
        "freshly_fetched"
    ) is not True:
        issues.append("source ledger does not prove fresh fetches")
    recorded_bound = as_nonnegative_int(get_path(ledger, "freshness", "required_within_seconds"))
    if recorded_bound is None or recorded_bound == 0 or recorded_bound > max_source_age_seconds:
        issues.append("source ledger freshness bound is missing, invalid, or weaker than readiness policy")

    sources = ledger.get("sources")
    if not isinstance(sources, dict):
        issues.append("source ledger sources is missing or malformed")
        sources = {}
    fetch_times: list[dt.datetime] = []
    for name in ("fork", "upstream"):
        source = sources.get(name)
        if not isinstance(source, dict):
            issues.append(f"source ledger {name} source is missing")
            continue
        fetched_sha = source.get("fetched_sha")
        if not is_full_sha(fetched_sha):
            issues.append(f"source ledger {name} fetched SHA is missing or malformed")
        if source_ref.get(f"{name}_sha") != fetched_sha:
            issues.append(f"patch queue {name} SHA does not match source ledger")
        canonical_url = source.get("canonical_url")
        configured_canonical_url = source.get("configured_canonical_url")
        if (
            not isinstance(canonical_url, str)
            or not canonical_url.strip()
            or canonical_url != configured_canonical_url
        ):
            issues.append(f"source ledger {name} canonical remote URL is missing or mismatched")
        history = source.get("history")
        if not isinstance(history, dict) or history.get("complete") is not True:
            issues.append(f"source ledger {name} history is incomplete")
        elif history.get("is_shallow_repository") is not False:
            issues.append(f"source ledger {name} shallow state is not full")
        fetched_at = parse_utc(get_path(source, "fetch", "fetched_at_utc"))
        if fetched_at is None:
            issues.append(f"source ledger {name} fetch timestamp is missing or malformed")
        else:
            fetch_times.append(fetched_at)
    local_source = ledger.get("local_source")
    issues.extend(validate_local_source_record(local_source))
    local_source_sha256 = (
        canonical_json_digest(local_source) if isinstance(local_source, dict) else None
    )
    if ledger.get("local_source_sha256") != local_source_sha256:
        issues.append("source ledger local-source record digest does not match its contents")
    if source_ref.get("local_source_sha256") != local_source_sha256:
        issues.append("patch queue local-source record digest does not match source ledger")
    local_tree_state_digest = (
        local_source.get("tree_state_digest") if isinstance(local_source, dict) else None
    )
    if source_ref.get("local_tree_state_digest") != local_tree_state_digest:
        issues.append("patch queue local tree-state digest does not match source ledger")
    fork_sha = get_path(sources, "fork", "fetched_sha")
    if isinstance(local_source, dict) and local_source.get("head_sha") != fork_sha:
        issues.append("source ledger local-source HEAD does not match fetched fork SHA")
    if isinstance(proof_queue_provenance, dict):
        if proof_queue_provenance.get("local_source_sha256") != local_source_sha256:
            issues.append("global proof local-source record digest does not match source ledger")
        if proof_queue_provenance.get("local_tree_state_digest") != local_tree_state_digest:
            issues.append("global proof local tree-state digest does not match source ledger")
    resampled_local_source, resample_errors = resample_local_source(repo_root)
    if resample_errors:
        issues.extend(
            f"canonical local-source resample reported: {message}" for message in resample_errors
        )
    if not isinstance(resampled_local_source, dict):
        issues.append("canonical local-source resample is missing or malformed")
    else:
        resampled_sha256 = canonical_json_digest(resampled_local_source)
        if resampled_sha256 != local_source_sha256:
            issues.append("current local-source record does not match the canonical source ledger")
        if resampled_local_source.get("tree_state_digest") != local_tree_state_digest:
            issues.append("current local tree-state digest does not match the source ledger")

    commit_range = ledger.get("range")
    merge_base = ledger.get("merge_base")
    merge_base_is_usable = (
        isinstance(merge_base, dict)
        and merge_base.get("status") == "present"
        and is_full_sha(merge_base.get("sha"))
    )
    reviewed_anchor = ledger.get("reviewed_anchor")
    anchor_is_usable = (
        isinstance(reviewed_anchor, dict)
        and reviewed_anchor.get("valid") is True
        and is_full_sha(reviewed_anchor.get("anchor_sha"))
        and is_full_sha(reviewed_anchor.get("evidence_fork_commit"))
        and isinstance(reviewed_anchor.get("review_basis"), dict)
        and isinstance(reviewed_anchor["review_basis"].get("kind"), str)
        and bool(reviewed_anchor["review_basis"]["kind"].strip())
        and isinstance(reviewed_anchor["review_basis"].get("semantic_evidence"), str)
        and bool(reviewed_anchor["review_basis"]["semantic_evidence"].strip())
        and isinstance(reviewed_anchor.get("evidence_artifacts"), list)
        and bool(reviewed_anchor.get("evidence_artifacts"))
    )
    if anchor_is_usable:
        anchor_sha = str(reviewed_anchor["anchor_sha"])
        for artifact in reviewed_anchor["evidence_artifacts"]:
            classification = artifact.get("anchor_classification") if isinstance(artifact, dict) else None
            if (
                not isinstance(artifact, dict)
                or not isinstance(artifact.get("path"), str)
                or not artifact["path"].strip()
                or not isinstance(classification, dict)
                or classification.get("sha") != anchor_sha
                or not isinstance(classification.get("disposition"), str)
                or not classification["disposition"].strip()
                or not isinstance(classification.get("classification_rule"), str)
                or not classification["classification_rule"].strip()
            ):
                anchor_is_usable = False
                issues.append("source ledger reviewed anchor exact evidence is malformed")
                break
    range_base = commit_range.get("base_ref") if isinstance(commit_range, dict) else None
    if merge_base_is_usable and range_base == merge_base.get("sha"):
        pass
    elif anchor_is_usable and range_base == reviewed_anchor.get("anchor_sha"):
        pass
    elif not merge_base_is_usable and not anchor_is_usable:
        issues.append("source ledger has neither an exact merge base nor a validated reviewed anchor")
    else:
        issues.append("source ledger range base is not proven by its merge base or reviewed anchor")
    if not isinstance(commit_range, dict):
        issues.append("source ledger range is missing or malformed")
        commit_range = {}
    range_shas = commit_range.get("commit_shas")
    if (
        commit_range.get("complete") is not True
        or not isinstance(range_shas, list)
        or not all(is_full_sha(sha) for sha in range_shas)
        or as_nonnegative_int(commit_range.get("commit_count")) != len(range_shas)
    ):
        issues.append("source ledger complete commit range is unavailable")
    expected_range_digest = hashlib.sha256("\n".join(range_shas or []).encode("ascii")).hexdigest()
    if commit_range.get("sha256") != expected_range_digest or source_ref.get("range_sha256") != expected_range_digest:
        issues.append("source ledger range digest does not match the queue")
    if queue_summary.get("complete_range_commit_count") != commit_range.get("commit_count"):
        issues.append("patch queue complete range count does not match source ledger")
    classifications = commit_range.get("classifications")
    if (
        not isinstance(classifications, list)
        or len(classifications) != len(range_shas or [])
        or any(
            not isinstance(record, dict)
            or record.get("sha") != sha
            or not isinstance(record.get("disposition"), str)
            or not isinstance(record.get("classification_rule"), str)
            for record, sha in zip(classifications, range_shas or [])
        )
    ):
        issues.append("source ledger complete classified range is unavailable")
    else:
        classification_digest = canonical_json_digest({"classifications": classifications})
        if (
            commit_range.get("classification_sha256") != classification_digest
            or source_ref.get("classification_sha256") != classification_digest
        ):
            issues.append("source ledger classification digest does not match the queue")
        queue_rows = queue.get("commits")
        if not isinstance(queue_rows, list) or len(queue_rows) != len(classifications):
            issues.append("patch queue rows do not cover the complete classified range")
        elif any(
            not isinstance(row, dict)
            or row.get("sha") != record.get("sha")
            or row.get("disposition") != record.get("disposition")
            or row.get("classification_rule") != record.get("classification_rule")
            or row.get("semantic_equivalence") != record.get("semantic_equivalence")
            for row, record in zip(queue_rows, classifications)
        ):
            issues.append("patch queue classifications do not match source ledger")

    now = dt.datetime.now(dt.timezone.utc)
    for fetched_at in fetch_times:
        age_seconds = (now - fetched_at).total_seconds()
        if age_seconds < -60 or age_seconds > max_source_age_seconds:
            issues.append("source ledger fetch timestamp is stale or in the future")
            break
    proof_generated = parse_utc(proof.get("generated_at_utc"))
    queue_generated = parse_utc(queue.get("generated_at_utc"))
    if proof_generated is None or queue_generated is None or proof_generated < queue_generated:
        issues.append("global proof is stale relative to patch queue")
    if proof.get("queue_summary") != queue_summary:
        issues.append("global proof queue summary does not match patch queue")
    if isinstance(metrics, dict):
        pending = as_nonnegative_int(get_path(queue_summary, "by_disposition", "pending"))
        if pending is not None and as_finite_number(metrics.get("max_queue_pending_commits")) != float(pending):
            issues.append("global proof pending-queue metric does not match patch queue")

    return {
        "pass": not issues,
        "issues": issues,
        "ledger_sha256": canonical_json_digest(ledger) if ledger else None,
        "fork_sha": fork_sha,
        "upstream_sha": get_path(sources, "upstream", "fetched_sha"),
        "local_source_sha256": local_source_sha256,
        "local_tree_state_digest": local_tree_state_digest,
    }


def build_upstream_delta_watch(
    source_integrity: dict[str, Any],
    queue_pending: int | None,
    release_gate_pass: bool,
) -> dict[str, Any]:
    pass_gate = source_integrity["pass"] and queue_pending == 0 and release_gate_pass
    return {
        "schema_version": 2,
        "pass": pass_gate,
        "status": "PASS" if pass_gate else "FAIL",
        "queue_pending": queue_pending,
        "release_gate_pass": release_gate_pass,
        "source_integrity_pass": bool(source_integrity["pass"]),
        "alerts": list(source_integrity["issues"]),
        "source_artifacts": [
            "docs/parity/global-parity-proof.json",
            "docs/parity/upstream-missing-queue.json",
            "docs/parity/upstream-source-ledger.json",
        ],
    }


def build_summary(
    repo_root: Path,
    max_source_age_seconds: int = 3600,
    additional_source_issues: list[str] | None = None,
) -> dict[str, Any]:
    artifact_issues: list[str] = []
    proof = load_json(
        repo_root / "docs/parity/global-parity-proof.json",
        "global parity gate report",
        artifact_issues,
    )
    queue = load_json(repo_root / "docs/parity/upstream-missing-queue.json", "patch queue", artifact_issues)
    ledger = load_json(repo_root / "docs/parity/upstream-source-ledger.json", "source ledger", artifact_issues)
    shared = load_json(repo_root / "docs/parity/shared-diff-backlog.json", "shared diff backlog", artifact_issues)
    coverage = load_json(repo_root / "docs/parity/test-coverage-audit.json", "coverage audit", artifact_issues)
    fixture_reference = load_json(
        repo_root / "docs/parity/sota-harness-matrix.json",
        "fixture-reference regression matrix",
        artifact_issues,
    )

    source_integrity = validate_source_integrity(
        repo_root,
        proof,
        queue,
        ledger,
        max_source_age_seconds,
    )
    source_integrity["issues"] = [
        *artifact_issues,
        *(additional_source_issues or []),
        *source_integrity["issues"],
    ]
    source_integrity["pass"] = not source_integrity["issues"]

    queue_pending = as_nonnegative_int(get_path(queue, "summary", "by_disposition", "pending"))
    queue_unowned = as_nonnegative_int(get_path(queue, "summary", "unowned_divergence_count"))
    shared_pending_classification = as_nonnegative_int(get_path(shared, "summary", "pending_classification"))
    shared_pending_review = as_nonnegative_int(get_path(shared, "summary", "pending_review"))
    coverage_critical = as_nonnegative_int(get_path(coverage, "audit_gate", "critical_gaps"))
    coverage_missing_refs = as_nonnegative_int(get_path(coverage, "summary", "missing_rust_test_refs"))
    fixture_authority = (
        fixture_reference.get("authority", {})
        if isinstance(fixture_reference.get("authority"), dict)
        else {}
    )
    fixture_scope = (
        fixture_reference.get("scope", {})
        if isinstance(fixture_reference.get("scope"), dict)
        else {}
    )
    fixture_gate = (
        fixture_reference.get("gate", {})
        if isinstance(fixture_reference.get("gate"), dict)
        else {}
    )
    fixture_reference_hygiene = {
        "coverage_kind": fixture_scope.get("coverage_kind", "missing"),
        "capability_promotion_authoritative": fixture_authority.get(
            "capability_promotion_authoritative", "missing"
        ),
        "production_complete": fixture_authority.get("production_complete", "missing"),
        "scope_valid": (
            fixture_authority.get("capability_promotion_authoritative") is False
            and fixture_authority.get("production_complete") is False
            and fixture_scope.get("coverage_kind") == "fixture_reference_regression"
            and fixture_gate.get("scope") == "fixture_reference_regression"
        ),
        "gate_pass": fixture_gate.get("pass") is True,
        "unresolved_gaps": as_nonnegative_int(fixture_gate.get("critical_gaps")),
        "missing_rust_test_refs": as_nonnegative_int(
            fixture_gate.get("missing_rust_test_refs")
        ),
    }
    release_gate_pass = get_path(proof, "release_gate", "pass") is True
    upstream_delta_watch = build_upstream_delta_watch(
        source_integrity,
        queue_pending,
        release_gate_pass,
    )

    def zero_check(check_id: str, label: str, actual: int | None) -> dict[str, Any]:
        return {
            "id": check_id,
            "label": label,
            "actual": actual if actual is not None else "missing",
            "limit": 0,
            "pass": actual == 0,
        }

    checks = [
        {
            "id": "source_integrity",
            "label": "Fresh source-ledger and proof provenance",
            "actual": "PASS" if source_integrity["pass"] else "FAIL",
            "pass": bool(source_integrity["pass"]),
        },
        {
            "id": "release_gate",
            "label": "Global release gate",
            "actual": "PASS" if release_gate_pass else "FAIL",
            "pass": release_gate_pass,
        },
        zero_check("queue_pending", "Upstream queue pending", queue_pending),
        zero_check("queue_unowned", "Unowned upstream divergence", queue_unowned),
        zero_check(
            "shared_pending_classification",
            "Shared diff pending classification",
            shared_pending_classification,
        ),
        zero_check("shared_pending_review", "Shared diff pending review", shared_pending_review),
        zero_check("coverage_critical_gaps", "Coverage critical gaps", coverage_critical),
        zero_check("coverage_missing_rust_refs", "Coverage missing Rust refs", coverage_missing_refs),
        {
            "id": "upstream_delta_watch",
            "label": "Upstream source truth",
            "actual": upstream_delta_watch["status"],
            "limit": "PASS",
            "pass": bool(upstream_delta_watch["pass"]),
        },
    ]
    return {
        "schema_version": 3,
        "workspace_version": workspace_version(repo_root),
        "head": get_path(ledger, "local_source", "head_sha"),
        "tag": git_output(repo_root, "describe", "--tags", "--exact-match", "HEAD"),
        "ok": all(bool(check["pass"]) for check in checks),
        "checks": checks,
        "source_artifacts": {
            "global_parity_proof": "docs/parity/global-parity-proof.json",
            "upstream_missing_queue": "docs/parity/upstream-missing-queue.json",
            "upstream_source_ledger": "docs/parity/upstream-source-ledger.json",
            "shared_diff_backlog": "docs/parity/shared-diff-backlog.json",
            "test_coverage_audit": "docs/parity/test-coverage-audit.json",
            "fixture_reference_matrix": "docs/parity/sota-harness-matrix.json",
        },
        "local_source": {
            "sha256": source_integrity.get("local_source_sha256"),
            "tree_state_digest": source_integrity.get("local_tree_state_digest"),
        },
        "summary": {
            "queue_pending": queue_pending,
            "queue_unowned_divergence": queue_unowned,
            "shared_pending_classification": shared_pending_classification,
            "shared_pending_review": shared_pending_review,
            "coverage_critical_gaps": coverage_critical,
            "coverage_missing_rust_refs": coverage_missing_refs,
            "fixture_reference_unresolved_gaps": fixture_reference_hygiene[
                "unresolved_gaps"
            ],
            "fixture_reference_missing_rust_refs": fixture_reference_hygiene[
                "missing_rust_test_refs"
            ],
            "release_gate_pass": release_gate_pass,
            "source_integrity_pass": bool(source_integrity["pass"]),
            "source_integrity_issue_count": len(source_integrity["issues"]),
            "upstream_delta_watch_pass": bool(upstream_delta_watch["pass"]),
            "upstream_delta_watch_status": upstream_delta_watch["status"],
        },
        "fixture_reference_hygiene": fixture_reference_hygiene,
        "source_integrity": source_integrity,
        "upstream_delta_watch": upstream_delta_watch,
    }


def render_markdown(summary: dict[str, Any]) -> str:
    status = "PASS" if summary["ok"] else "FAIL"
    lines = [
        "# Hermes Agent Ultra Release Readiness Summary",
        "",
        f"Status: **{status}**",
        f"Workspace version: `{summary['workspace_version']}`",
        f"HEAD: `{str(summary.get('head') or '')[:12]}`",
    ]
    if summary.get("tag"):
        lines.append(f"Tag: `{summary['tag']}`")
    lines.extend(["", "## Gates", "", "| Gate | Actual | Limit | Status |", "| --- | ---: | ---: | --- |"])
    for check in summary["checks"]:
        limit = check.get("limit", "PASS")
        lines.append(
            f"| {check['label']} | `{check['actual']}` | `{limit}` | "
            f"{'PASS' if check['pass'] else 'FAIL'} |"
        )
    fixture_hygiene = summary["fixture_reference_hygiene"]
    lines.extend(
        [
            "",
            "## Fixture-Reference Regression Hygiene (Non-Authoritative)",
            "",
            f"- Coverage scope: `{fixture_hygiene['coverage_kind']}`",
            "- Registry/fixture entries and referenced Rust tests preserve regression coverage only. They do not establish an implemented product capability or production completeness.",
            f"- Capability-promotion authority: `{str(fixture_hygiene['capability_promotion_authoritative']).lower()}`",
            f"- Production complete: `{str(fixture_hygiene['production_complete']).lower()}`",
            f"- Scope valid: `{str(fixture_hygiene['scope_valid']).lower()}`",
            f"- Fixture/reference gate: `{'PASS' if fixture_hygiene['gate_pass'] else 'FAIL'}`",
            f"- Unresolved fixture-reference gaps: `{fixture_hygiene['unresolved_gaps']}`",
            f"- Missing referenced Rust test refs: `{fixture_hygiene['missing_rust_test_refs']}`",
        ]
    )
    lines.extend(["", "## Source Integrity", ""])
    integrity = summary["source_integrity"]
    lines.append(f"- Status: `{'PASS' if integrity['pass'] else 'FAIL'}`")
    lines.append(f"- Canonical ledger digest: `{integrity.get('ledger_sha256') or 'missing'}`")
    lines.append(f"- Fetched fork SHA: `{integrity.get('fork_sha') or 'missing'}`")
    lines.append(f"- Fetched upstream SHA: `{integrity.get('upstream_sha') or 'missing'}`")
    if integrity["issues"]:
        lines.append("- Fail-closed reasons:")
        lines.extend(f"  - {issue}" for issue in integrity["issues"])
    lines.extend(["", "## Source Artifacts", ""])
    for label, path in summary["source_artifacts"].items():
        lines.append(f"- `{label}`: `{path}`")
    watch = summary["upstream_delta_watch"]
    lines.extend([
        "",
        "## Upstream Source Truth",
        "",
        f"- Status: `{watch['status']}`",
        f"- Queue pending: `{watch['queue_pending']}`",
        f"- Release gate pass: `{watch['release_gate_pass']}`",
        f"- Source integrity pass: `{watch['source_integrity_pass']}`",
    ])
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--output-json", default=".sync-reports/release-readiness-summary.json")
    parser.add_argument("--output-md", default=".sync-reports/release-readiness-summary.md")
    parser.add_argument("--max-source-age-seconds", type=int, default=3600)
    parser.add_argument("--check", action="store_true", help="Exit nonzero if release readiness gates fail")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    output_json_arg = Path(args.output_json)
    output_md_arg = Path(args.output_md)
    output_issues = [
        f"{label}: {error}"
        for label, candidate in (
            ("readiness JSON", output_json_arg),
            ("readiness markdown", output_md_arg),
        )
        if (error := generated_evidence_output_error(repo_root, candidate))
    ]
    summary = build_summary(
        repo_root,
        args.max_source_age_seconds,
        additional_source_issues=output_issues,
    )
    output_json = output_json_arg if output_json_arg.is_absolute() else repo_root / output_json_arg
    output_md = output_md_arg if output_md_arg.is_absolute() else repo_root / output_md_arg
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    output_md.write_text(render_markdown(summary), encoding="utf-8")
    print(f"Wrote {output_json}")
    print(f"Wrote {output_md}")
    print(f"release_readiness={'PASS' if summary['ok'] else 'FAIL'}")
    return 1 if args.check and not summary["ok"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
