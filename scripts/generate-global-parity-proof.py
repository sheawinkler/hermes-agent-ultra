#!/usr/bin/env python3
"""Generate a comprehensive parity proof artifact and evaluate thresholds."""

from __future__ import annotations

import argparse
import importlib.util
import datetime as dt
import hashlib
import json
import math
from functools import lru_cache
import re
from pathlib import Path
from typing import Any

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
NON_EVIDENCE_GATE_METRICS = {
    "max_commits_behind",
    "max_upstream_patch_missing",
}


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


def public_repo_relative_path(repo_root: Path, path: Path) -> str:
    candidate = path if path.is_absolute() else repo_root / path
    return str(_local_source_contract_module().public_repo_relative_path(candidate, repo_root))


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


def require_numeric_metric(
    payload: dict[str, Any],
    path: tuple[str, ...],
    label: str,
) -> str | None:
    current: Any = payload
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return f"{label} is missing"
        current = current[key]
    if isinstance(current, bool):
        return f"{label} must be numeric, not boolean"
    try:
        value = float(current)
    except (TypeError, ValueError):
        return f"{label} is not numeric"
    if not math.isfinite(value):
        return f"{label} is not finite"
    return None


def validate_queue_provenance(
    repo_root: Path,
    patch_queue: dict[str, Any],
    source_ledger_path: Path,
    max_source_age_seconds: int,
) -> dict[str, Any]:
    """Require a current, complete queue ledger before either proof gate can pass."""

    errors: list[str] = []
    if patch_queue.get("schema_version") != 2:
        errors.append("patch queue schema_version must be 2")
    source = patch_queue.get("source_ledger")
    if not isinstance(source, dict):
        errors.append("patch queue has no source_ledger object")
        source = {}
    expected_relative = "docs/parity/upstream-source-ledger.json"
    if source.get("path") != expected_relative:
        errors.append("patch queue does not point at the canonical source ledger path")
    try:
        ledger = load_json(source_ledger_path)
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        errors.append(sanitize_public_message(f"source ledger is unreadable: {exc}"))
        ledger = {}
    if ledger.get("schema_version") != 2:
        errors.append("source ledger schema_version must be 2")
    generator = ledger.get("generator")
    if not isinstance(generator, dict) or not str(generator.get("version", "")).strip():
        errors.append("source ledger generator provenance is missing")
    if ledger.get("source_valid") is not True:
        errors.append("source ledger is not valid")
    ledger_errors = ledger.get("source_errors")
    if not isinstance(ledger_errors, list):
        errors.append("source ledger source_errors is missing or malformed")
    elif ledger_errors:
        errors.append("source ledger reports source or classification errors")
    freshness = ledger.get("freshness")
    if not isinstance(freshness, dict) or freshness.get("state") != "fresh" or freshness.get(
        "freshly_fetched"
    ) is not True:
        errors.append("source ledger does not prove fresh fetches")
    if max_source_age_seconds <= 0:
        errors.append("configured source freshness bound must be positive")
    recorded_freshness_bound: int | None = None
    if isinstance(freshness, dict):
        raw_bound = freshness.get("required_within_seconds")
        if isinstance(raw_bound, bool):
            raw_bound = None
        try:
            recorded_freshness_bound = int(raw_bound)
        except (TypeError, ValueError):
            recorded_freshness_bound = None
    if (
        recorded_freshness_bound is None
        or recorded_freshness_bound <= 0
        or recorded_freshness_bound > max_source_age_seconds
    ):
        errors.append("source ledger freshness bound is missing, invalid, or weaker than proof policy")
    now = dt.datetime.now(dt.timezone.utc)
    ledger_sources = ledger.get("sources")
    if not isinstance(ledger_sources, dict):
        errors.append("source ledger sources is missing or malformed")
        ledger_sources = {}
    for name in ("fork", "upstream"):
        item = ledger_sources.get(name)
        if not isinstance(item, dict):
            errors.append(f"source ledger {name} source is missing")
            continue
        if not is_full_sha(item.get("fetched_sha")):
            errors.append(f"source ledger {name} fetched SHA is missing or malformed")
        canonical_url = item.get("canonical_url")
        configured_canonical_url = item.get("configured_canonical_url")
        if (
            not isinstance(canonical_url, str)
            or not canonical_url.strip()
            or canonical_url != configured_canonical_url
        ):
            errors.append(f"source ledger {name} canonical remote URL is missing or mismatched")
        history = item.get("history")
        if not isinstance(history, dict) or history.get("complete") is not True:
            errors.append(f"source ledger {name} history is incomplete")
        elif history.get("is_shallow_repository") is not False:
            errors.append(f"source ledger {name} shallow state is not full")
        fetch = item.get("fetch")
        fetched_at = parse_utc(fetch.get("fetched_at_utc") if isinstance(fetch, dict) else None)
        if fetched_at is None:
            errors.append(f"source ledger {name} fetch timestamp is missing or malformed")
        elif (
            (age_seconds := (now - fetched_at).total_seconds()) < -60
            or age_seconds > max_source_age_seconds
        ):
            errors.append(f"source ledger {name} fetch timestamp is stale or in the future")
    local_source = ledger.get("local_source")
    errors.extend(validate_local_source_record(local_source))
    local_source_sha256 = (
        canonical_json_digest(local_source) if isinstance(local_source, dict) else None
    )
    if ledger.get("local_source_sha256") != local_source_sha256:
        errors.append("source ledger local-source record digest does not match its contents")
    if source.get("local_source_sha256") != local_source_sha256:
        errors.append("patch queue local-source record digest does not match source ledger")
    local_tree_state_digest = (
        local_source.get("tree_state_digest") if isinstance(local_source, dict) else None
    )
    if source.get("local_tree_state_digest") != local_tree_state_digest:
        errors.append("patch queue local tree-state digest does not match source ledger")
    fork_sha = ledger_sources.get("fork", {}).get("fetched_sha")
    if isinstance(local_source, dict) and local_source.get("head_sha") != fork_sha:
        errors.append("source ledger local-source HEAD does not match fetched fork SHA")
    resampled_local_source, resample_errors = resample_local_source(repo_root)
    if resample_errors:
        errors.extend(
            f"canonical local-source resample reported: {message}" for message in resample_errors
        )
    if not isinstance(resampled_local_source, dict):
        errors.append("canonical local-source resample is missing or malformed")
    else:
        resampled_sha256 = canonical_json_digest(resampled_local_source)
        if resampled_sha256 != local_source_sha256:
            errors.append("current local-source record does not match the canonical source ledger")
        if resampled_local_source.get("tree_state_digest") != local_tree_state_digest:
            errors.append("current local tree-state digest does not match the source ledger")
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
                errors.append("source ledger reviewed anchor exact evidence is malformed")
                break
    range_base = commit_range.get("base_ref") if isinstance(commit_range, dict) else None
    if merge_base_is_usable and range_base == merge_base.get("sha"):
        pass
    elif anchor_is_usable and range_base == reviewed_anchor.get("anchor_sha"):
        pass
    elif not merge_base_is_usable and not anchor_is_usable:
        errors.append("source ledger has neither an exact merge base nor a validated reviewed anchor")
    else:
        errors.append("source ledger range base is not proven by its merge base or reviewed anchor")
    if (
        not isinstance(commit_range, dict)
        or commit_range.get("complete") is not True
        or not isinstance(commit_range.get("commit_shas"), list)
        or not all(is_full_sha(sha) for sha in commit_range.get("commit_shas", []))
    ):
        errors.append("source ledger complete classified range is unavailable")
    if source.get("sha256") != canonical_json_digest(ledger):
        errors.append("patch queue source-ledger digest does not match the ledger")
    for name in ("fork", "upstream"):
        expected_sha = ledger_sources.get(name, {}).get("fetched_sha")
        if source.get(f"{name}_sha") != expected_sha:
            errors.append(f"patch queue {name} SHA does not match source ledger")
    if source.get("range_sha256") != (commit_range or {}).get("sha256"):
        errors.append("patch queue range digest does not match source ledger")
    classifications = (commit_range or {}).get("classifications")
    range_shas = (commit_range or {}).get("commit_shas", [])
    if (
        not isinstance(classifications, list)
        or len(classifications) != len(range_shas)
        or any(
            not isinstance(record, dict)
            or record.get("sha") != sha
            or not isinstance(record.get("disposition"), str)
            or not isinstance(record.get("classification_rule"), str)
            for record, sha in zip(classifications, range_shas)
        )
    ):
        errors.append("source ledger complete classified range is malformed")
    else:
        classification_digest = canonical_json_digest({"classifications": classifications})
        if (commit_range or {}).get("classification_sha256") != classification_digest:
            errors.append("source ledger classification digest is malformed")
        if source.get("classification_sha256") != classification_digest:
            errors.append("patch queue classification digest does not match source ledger")
        queue_rows = patch_queue.get("commits")
        if not isinstance(queue_rows, list) or len(queue_rows) != len(classifications):
            errors.append("patch queue rows do not cover the complete classified range")
        elif any(
            not isinstance(row, dict)
            or row.get("sha") != record.get("sha")
            or row.get("disposition") != record.get("disposition")
            or row.get("classification_rule") != record.get("classification_rule")
            or row.get("semantic_equivalence") != record.get("semantic_equivalence")
            for row, record in zip(queue_rows, classifications)
        ):
            errors.append("patch queue classifications do not match source ledger")
    truth = patch_queue.get("truth")
    if not isinstance(truth, dict) or truth.get("pass") is not True:
        errors.append("patch queue truth status is not PASS")
    elif truth.get("error_count") != 0 or truth.get("errors") not in ([], None):
        errors.append("patch queue truth status has unacknowledged errors")
    summary = patch_queue.get("summary")
    if not isinstance(summary, dict):
        errors.append("patch queue summary is missing or malformed")
    else:
        for key in (
            "total_commits",
            "complete_range_commit_count",
            "unowned_divergence_count",
        ):
            error = require_numeric_metric(summary, (key,), f"patch queue summary.{key}")
            if error:
                errors.append(error)
        if not isinstance(summary.get("by_disposition"), dict):
            errors.append("patch queue summary.by_disposition is missing or malformed")
        elif require_numeric_metric(
            summary,
            ("by_disposition", "pending"),
            "patch queue summary.by_disposition.pending",
        ):
            errors.append("patch queue summary.by_disposition.pending is missing or malformed")
        if (
            isinstance(commit_range, dict)
            and summary.get("complete_range_commit_count") != commit_range.get("commit_count")
        ):
            errors.append("patch queue complete range count does not match source ledger")
    return {
        "pass": not errors,
        "errors": errors,
        "ledger_path": public_repo_relative_path(repo_root, source_ledger_path),
        "ledger_sha256": canonical_json_digest(ledger) if ledger else None,
        "fork_sha": ledger_sources.get("fork", {}).get("fetched_sha"),
        "upstream_sha": ledger_sources.get("upstream", {}).get("fetched_sha"),
        "local_source_sha256": local_source_sha256,
        "local_tree_state_digest": local_tree_state_digest,
    }


def parity_matrix_source_errors(
    parity: dict[str, Any],
    queue_provenance: dict[str, Any],
) -> list[str]:
    refs = parity.get("refs")
    if not isinstance(refs, dict):
        return ["parity matrix source refs are missing or malformed"]
    errors: list[str] = []
    if refs.get("local_sha") != queue_provenance.get("fork_sha"):
        errors.append("parity matrix local SHA is stale relative to the canonical source ledger")
    if refs.get("upstream_sha") != queue_provenance.get("upstream_sha"):
        errors.append("parity matrix upstream SHA is stale relative to the canonical source ledger")
    return errors


def append_truth_failure(gate: dict[str, Any], metric: str, errors: list[str]) -> None:
    if not errors:
        return
    gate["pass"] = False
    gate["checks"].append(
        {
            "metric": metric,
            "status": "fail",
            "actual": errors,
            "limit": "no source/provenance/metric errors",
        }
    )


def required_source_metric_errors(
    parity_summary: dict[str, Any],
    divergence_summary: dict[str, Any],
    intent_summary: dict[str, Any],
    test_coverage_summary: dict[str, Any],
    test_coverage_gate: dict[str, Any],
    sota_harness_summary: dict[str, Any],
    sota_harness_gate: dict[str, Any],
    behavioral_summary: dict[str, Any],
    deep_summary: dict[str, Any],
) -> list[str]:
    required = (
        (parity_summary, ("commits_behind",), "parity_matrix.summary.commits_behind"),
        (
            parity_summary,
            ("upstream_patch_missing",),
            "parity_matrix.summary.upstream_patch_missing",
        ),
        (parity_summary, ("files_only_upstream",), "parity_matrix.summary.files_only_upstream"),
        (divergence_summary, ("unowned",), "divergence_validation.summary.unowned"),
        (
            divergence_summary,
            ("review_overdue",),
            "divergence_validation.summary.review_overdue",
        ),
        (intent_summary, ("mapping_ratio",), "test_intent_mapping.summary.mapping_ratio"),
        (
            test_coverage_summary,
            ("tracked_behavior_coverage_ratio",),
            "test_coverage_audit.summary.tracked_behavior_coverage_ratio",
        ),
        (
            test_coverage_gate,
            ("critical_gaps",),
            "test_coverage_audit.audit_gate.critical_gaps",
        ),
        (
            test_coverage_summary,
            ("missing_rust_test_refs",),
            "test_coverage_audit.summary.missing_rust_test_refs",
        ),
        (
            sota_harness_summary,
            ("domain_coverage_ratio",),
            "fixture_reference_matrix.summary.domain_coverage_ratio",
        ),
        (
            sota_harness_gate,
            ("critical_gaps",),
            "fixture_reference_matrix.gate.critical_gaps",
        ),
        (
            sota_harness_gate,
            ("missing_rust_test_refs",),
            "fixture_reference_matrix.gate.missing_rust_test_refs",
        ),
        (
            behavioral_summary,
            ("behavioral_similarity_ratio",),
            "behavioral_similarity_diff.summary.behavioral_similarity_ratio",
        ),
        (
            behavioral_summary,
            ("superior_cases",),
            "behavioral_similarity_diff.summary.superior_cases",
        ),
        (
            behavioral_summary,
            ("regressions",),
            "behavioral_similarity_diff.summary.regressions",
        ),
        (behavioral_summary, ("gaps",), "behavioral_similarity_diff.summary.gaps"),
        (
            behavioral_summary,
            ("unverified_cases",),
            "behavioral_similarity_diff.summary.unverified_cases",
        ),
        (
            behavioral_summary,
            ("missing_rust_test_refs",),
            "behavioral_similarity_diff.summary.missing_rust_test_refs",
        ),
        (
            deep_summary,
            ("deep_problem_solving_ratio",),
            "deep_problem_solving_diff.summary.deep_problem_solving_ratio",
        ),
        (
            deep_summary,
            ("superior_cases",),
            "deep_problem_solving_diff.summary.superior_cases",
        ),
        (
            deep_summary,
            ("total_cases",),
            "deep_problem_solving_diff.summary.total_cases",
        ),
        (deep_summary, ("regressions",), "deep_problem_solving_diff.summary.regressions"),
        (deep_summary, ("gaps",), "deep_problem_solving_diff.summary.gaps"),
        (
            deep_summary,
            ("unverified_cases",),
            "deep_problem_solving_diff.summary.unverified_cases",
        ),
        (
            deep_summary,
            ("missing_rust_test_refs",),
            "deep_problem_solving_diff.summary.missing_rust_test_refs",
        ),
    )
    return [
        error
        for payload, path, label in required
        if (error := require_numeric_metric(payload, path, label)) is not None
    ]


def fixture_reference_scope_errors(
    authority: dict[str, Any],
    scope: dict[str, Any],
    gate: dict[str, Any],
) -> list[str]:
    errors: list[str] = []
    if authority.get("capability_promotion_authoritative") is not False:
        errors.append(
            "fixture_reference_matrix.authority.capability_promotion_authoritative must be false"
        )
    if authority.get("production_complete") is not False:
        errors.append("fixture_reference_matrix.authority.production_complete must be false")
    if scope.get("coverage_kind") != "fixture_reference_regression":
        errors.append(
            "fixture_reference_matrix.scope.coverage_kind must be fixture_reference_regression"
        )
    if gate.get("scope") != "fixture_reference_regression":
        errors.append(
            "fixture_reference_matrix.gate.scope must be fixture_reference_regression"
        )
    return errors

def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def gate_mode(thresholds: dict[str, Any]) -> str:
    mode = str(thresholds.get("gate_mode", "")).strip().lower()
    return mode if mode else "legacy"


def gate_metric_thresholds(thresholds: dict[str, Any]) -> dict[str, Any]:
    metric_thresholds = thresholds.get("metric_thresholds")
    if isinstance(metric_thresholds, dict):
        raw = metric_thresholds
    else:
        raw = {
            key: value
            for key, value in thresholds.items()
            if key not in {"required_workstreams_complete", "gate_mode", "special_rules"}
        }
    # Commit reachability counts are retained as drift observations, never as
    # functional parity evidence or release/CI gate inputs.
    return {
        key: value
        for key, value in raw.items()
        if key not in NON_EVIDENCE_GATE_METRICS
    }


def evaluate_gate(metrics: dict[str, float], thresholds: dict[str, Any]) -> dict[str, Any]:
    checks: list[dict[str, Any]] = []
    passed = True
    for key, limit in gate_metric_thresholds(thresholds).items():
        actual = metrics.get(key)
        if actual is None:
            checks.append({"metric": key, "status": "missing", "actual": None, "limit": limit})
            passed = False
            continue
        if key.startswith("min_"):
            ok = actual >= float(limit)
            checks.append({"metric": key, "status": "pass" if ok else "fail", "actual": actual, "limit": limit})
            passed = passed and ok
        else:
            ok = actual <= float(limit)
            checks.append({"metric": key, "status": "pass" if ok else "fail", "actual": actual, "limit": limit})
            passed = passed and ok
    return {"pass": passed, "checks": checks}


def _check_by_metric(checks: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    out: dict[str, dict[str, Any]] = {}
    for check in checks:
        metric = str(check.get("metric", "")).strip()
        if metric:
            out[metric] = check
    return out


def apply_ci_special_rules(
    metrics: dict[str, float],
    ci_gate: dict[str, Any],
    release_gate: dict[str, Any],
    ci_thresholds: dict[str, Any],
) -> None:
    special_rules = ci_thresholds.get("special_rules")
    if not isinstance(special_rules, dict):
        return

    tree_rule_cfg = special_rules.get("allow_tree_drift_when_functional_clean")
    if isinstance(tree_rule_cfg, dict) and bool(tree_rule_cfg.get("enabled", False)):
        checks = ci_gate.get("checks", [])
        if isinstance(checks, list):
            failed_checks = [c for c in checks if c.get("status") == "fail"]
            failed_metrics = {str(c.get("metric", "")).strip() for c in failed_checks}
            allowed_metrics_raw = tree_rule_cfg.get("metrics", [])
            allowed_metrics = {
                normalized
                for metric in allowed_metrics_raw
                if (normalized := str(metric).strip())
                and normalized not in NON_EVIDENCE_GATE_METRICS
            }
            release_ok = (not bool(tree_rule_cfg.get("requires_release_gate_pass", True))) or bool(
                release_gate.get("pass", False)
            )
            queue_limit = float(tree_rule_cfg.get("requires_max_queue_pending_commits", 0))
            unowned_limit = float(tree_rule_cfg.get("requires_max_unowned_divergences", 0))
            review_limit = float(tree_rule_cfg.get("requires_max_divergence_review_overdue", 0))
            coverage_ratio = float(
                tree_rule_cfg.get("requires_min_test_coverage_tracked_behavior_ratio", 1.0)
            )
            sota_ratio = float(
                tree_rule_cfg.get(
                    "requires_min_fixture_reference_category_coverage_ratio", 1.0
                )
            )
            behavioral_ratio = float(
                tree_rule_cfg.get("requires_min_behavioral_similarity_ratio", 1.0)
            )
            behavioral_regression_limit = float(
                tree_rule_cfg.get("requires_max_behavioral_regressions", 0)
            )
            behavioral_unverified_limit = float(
                tree_rule_cfg.get("requires_max_behavioral_unverified_cases", 0)
            )
            deep_ratio = float(
                tree_rule_cfg.get("requires_min_deep_problem_solving_ratio", 1.0)
            )
            deep_gap_limit = float(
                tree_rule_cfg.get("requires_max_deep_problem_solving_gaps", 0)
            )
            deep_unverified_limit = float(
                tree_rule_cfg.get("requires_max_deep_problem_solving_unverified_cases", 0)
            )
            if (
                failed_metrics
                and failed_metrics.issubset(allowed_metrics)
                and release_ok
                and metrics.get("max_queue_pending_commits", 0.0) <= queue_limit
                and metrics.get("max_unowned_divergences", 0.0) <= unowned_limit
                and metrics.get("max_divergence_review_overdue", 0.0) <= review_limit
                and metrics.get("min_test_coverage_tracked_behavior_ratio", 0.0) >= coverage_ratio
                and metrics.get("min_fixture_reference_category_coverage_ratio", 0.0)
                >= sota_ratio
                and metrics.get("min_behavioral_similarity_ratio", 0.0) >= behavioral_ratio
                and metrics.get("max_behavioral_regressions", 0.0)
                <= behavioral_regression_limit
                and metrics.get("max_behavioral_unverified_cases", 0.0)
                <= behavioral_unverified_limit
                and metrics.get("min_deep_problem_solving_ratio", 0.0) >= deep_ratio
                and metrics.get("max_deep_problem_solving_gaps", 0.0)
                <= deep_gap_limit
                and metrics.get("max_deep_problem_solving_unverified_cases", 0.0)
                <= deep_unverified_limit
            ):
                for check in failed_checks:
                    check["status"] = "warn"
                    check["special_rule"] = "allow_tree_drift_when_functional_clean"
                ci_gate["pass"] = True
                ci_gate.setdefault("special_rules_applied", []).append(
                    {
                        "rule": "allow_tree_drift_when_functional_clean",
                        "metrics": sorted(failed_metrics),
                        "reason": (
                            "functional parity clean; raw upstream tree drift kept as "
                            "observability warning for the Rust fork"
                        ),
                    }
                )
                return

    rule_cfg = special_rules.get("allow_files_only_upstream_overshoot_when_functional_clean")
    if not isinstance(rule_cfg, dict):
        return
    if not bool(rule_cfg.get("enabled", False)):
        return

    checks = ci_gate.get("checks", [])
    if not isinstance(checks, list):
        return
    failed_checks = [c for c in checks if c.get("status") == "fail"]
    failed_metrics = {str(c.get("metric", "")).strip() for c in failed_checks}
    if failed_metrics != {"max_files_only_upstream"}:
        return

    check_map = _check_by_metric(checks)
    files_only_upstream_check = check_map.get("max_files_only_upstream")
    if not isinstance(files_only_upstream_check, dict):
        return

    actual_raw = files_only_upstream_check.get("actual")
    limit_raw = files_only_upstream_check.get("limit")
    try:
        actual = float(actual_raw)
        limit = float(limit_raw)
    except (TypeError, ValueError):
        return

    overshoot = max(0.0, actual - limit)
    max_overshoot = float(rule_cfg.get("max_overshoot", 0.0))
    if overshoot > max_overshoot:
        return

    if bool(rule_cfg.get("requires_release_gate_pass", True)) and not bool(
        release_gate.get("pass", False)
    ):
        return

    unowned_limit = float(rule_cfg.get("requires_max_unowned_divergences", 0))
    review_limit = float(rule_cfg.get("requires_max_divergence_review_overdue", 0))
    pending_limit = float(rule_cfg.get("requires_max_queue_pending_commits", 100))
    ratio_limit = float(rule_cfg.get("requires_min_test_intent_mapping_ratio", 0.9))
    if metrics.get("max_unowned_divergences", 0.0) > unowned_limit:
        return
    if metrics.get("max_divergence_review_overdue", 0.0) > review_limit:
        return
    if metrics.get("max_queue_pending_commits", 0.0) > pending_limit:
        return
    if metrics.get("min_test_intent_mapping_ratio", 0.0) < ratio_limit:
        return

    files_only_upstream_check["status"] = "warn"
    files_only_upstream_check["special_rule"] = (
        "allow_files_only_upstream_overshoot_when_functional_clean"
    )
    files_only_upstream_check["overshoot"] = overshoot
    files_only_upstream_check["max_overshoot"] = max_overshoot
    ci_gate["pass"] = True
    ci_gate.setdefault("special_rules_applied", []).append(
        {
            "rule": "allow_files_only_upstream_overshoot_when_functional_clean",
            "metric": "max_files_only_upstream",
            "actual": actual,
            "limit": limit,
            "overshoot": overshoot,
            "max_overshoot": max_overshoot,
            "reason": "functional parity clean; upstream-only file growth treated as drift warning",
        }
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate global parity gate report."
    )
    parser.add_argument("--repo-root", default=".", help="Repository root path")
    parser.add_argument(
        "--thresholds",
        default="docs/parity/global-parity-thresholds.json",
        type=Path,
        help="Parity thresholds file (relative to repo root)",
    )
    parser.add_argument(
        "--parity-matrix",
        default="docs/parity/parity-matrix.json",
        type=Path,
        help="Parity matrix JSON file",
    )
    parser.add_argument(
        "--workstream-status",
        default="docs/parity/workstream-status.json",
        type=Path,
        help="Workstream status JSON file",
    )
    parser.add_argument(
        "--intent-mapping",
        default="docs/parity/test-intent-mapping.json",
        type=Path,
        help="Test intent mapping JSON file",
    )
    parser.add_argument(
        "--adapter-matrix",
        default="docs/parity/adapter-feature-matrix.json",
        type=Path,
        help="Adapter feature matrix JSON file",
    )
    parser.add_argument(
        "--shared-diff-classification",
        default="docs/parity/shared-different-classification.json",
        type=Path,
        help="Shared-different classification JSON file",
    )
    parser.add_argument(
        "--divergence-validation",
        default="docs/parity/divergence-validation.json",
        type=Path,
        help="Divergence validation report JSON file",
    )
    parser.add_argument(
        "--patch-queue",
        default="docs/parity/upstream-missing-queue.json",
        type=Path,
        help="Upstream patch queue JSON file",
    )
    parser.add_argument(
        "--source-ledger",
        default="docs/parity/upstream-source-ledger.json",
        type=Path,
        help="Canonical fresh upstream/fork source ledger JSON file",
    )
    parser.add_argument(
        "--test-coverage-audit",
        default="docs/parity/test-coverage-audit.json",
        type=Path,
        help="Test coverage audit JSON file",
    )
    parser.add_argument(
        "--sota-harness-matrix",
        default="docs/parity/sota-harness-matrix.json",
        type=Path,
        help="Fixture-reference regression matrix JSON file",
    )
    parser.add_argument(
        "--behavioral-diff",
        default="docs/parity/behavioral-similarity-diff.json",
        type=Path,
        help="Behavioral similarity diff JSON file",
    )
    parser.add_argument(
        "--deep-problem-solving-diff",
        default="docs/parity/deep-problem-solving-diff.json",
        type=Path,
        help="Deep problem-solving diff JSON file",
    )
    parser.add_argument(
        "--out-json",
        default="docs/parity/global-parity-proof.json",
        type=Path,
        help="Output proof JSON path",
    )
    parser.add_argument(
        "--out-md",
        default="docs/parity/global-parity-proof.md",
        type=Path,
        help="Output proof markdown path",
    )
    parser.add_argument(
        "--max-source-age-seconds",
        type=int,
        default=3600,
        help="Maximum accepted age for source-ledger fetch timestamps",
    )
    parser.add_argument("--check-ci", action="store_true", help="Fail if CI gate fails")
    parser.add_argument("--check-release", action="store_true", help="Fail if release gate fails")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    output_errors = [
        f"{label}: {error}"
        for label, candidate in (("proof JSON", args.out_json), ("proof markdown", args.out_md))
        if (error := generated_evidence_output_error(repo_root, candidate))
    ]
    thresholds = load_json((repo_root / args.thresholds).resolve())
    parity = load_json((repo_root / args.parity_matrix).resolve())
    ws = load_json((repo_root / args.workstream_status).resolve())
    intents = load_json((repo_root / args.intent_mapping).resolve())
    adapters = load_json((repo_root / args.adapter_matrix).resolve())
    shared_diff = load_json((repo_root / args.shared_diff_classification).resolve())
    divergence = load_json((repo_root / args.divergence_validation).resolve())
    patch_queue = load_json((repo_root / args.patch_queue).resolve())
    test_coverage = load_json((repo_root / args.test_coverage_audit).resolve())
    sota_harness = load_json((repo_root / args.sota_harness_matrix).resolve())
    behavioral_diff = load_json((repo_root / args.behavioral_diff).resolve())
    deep_problem_solving = load_json((repo_root / args.deep_problem_solving_diff).resolve())
    source_ledger_path = (repo_root / args.source_ledger).resolve()
    queue_provenance = validate_queue_provenance(
        repo_root,
        patch_queue,
        source_ledger_path,
        args.max_source_age_seconds,
    )

    parity_summary = parity.get("summary", {})
    intent_summary = intents.get("summary", {})
    adapter_summary = adapters.get("summary", {})
    divergence_summary = divergence.get("summary", {})
    queue_summary = patch_queue.get("summary", {})
    test_coverage_summary = test_coverage.get("summary", {})
    test_coverage_gate = test_coverage.get("audit_gate", {})
    sota_harness_summary = sota_harness.get("summary", {})
    sota_harness_gate = sota_harness.get("gate", {})
    fixture_reference_authority = sota_harness.get("authority", {})
    fixture_reference_scope = sota_harness.get("scope", {})
    behavioral_summary = behavioral_diff.get("summary", {})
    behavioral_gate = behavioral_diff.get("gate", {})
    deep_summary = deep_problem_solving.get("summary", {})
    deep_gate = deep_problem_solving.get("gate", {})
    ws_states = ws.get("states", {})
    shared_diff_items = {str(i.get("path", "")) for i in shared_diff.get("items", [])}
    parity_shared = {str(i.get("path", "")) for i in parity.get("top_shared_different", [])}
    source_metric_errors = required_source_metric_errors(
        parity_summary,
        divergence_summary,
        intent_summary,
        test_coverage_summary,
        test_coverage_gate,
        sota_harness_summary,
        sota_harness_gate,
        behavioral_summary,
        deep_summary,
    )
    fixture_reference_errors = fixture_reference_scope_errors(
        fixture_reference_authority
        if isinstance(fixture_reference_authority, dict)
        else {},
        fixture_reference_scope if isinstance(fixture_reference_scope, dict) else {},
        sota_harness_gate if isinstance(sota_harness_gate, dict) else {},
    )
    parity_provenance_errors = parity_matrix_source_errors(parity, queue_provenance)
    proof_truth_errors = [
        *output_errors,
        *queue_provenance["errors"],
        *parity_provenance_errors,
        *source_metric_errors,
        *fixture_reference_errors,
    ]
    proof_truth = {
        "pass": not proof_truth_errors,
        "errors": proof_truth_errors,
        "queue_provenance": queue_provenance,
        "missing_metric_count": len(source_metric_errors),
        "stale_source_count": len(parity_provenance_errors),
    }

    gpar_completion = {
        "GPAR-01": float(intent_summary.get("mapping_ratio", 0.0)) >= 0.9,
        "GPAR-02": any(i.get("id") == "skills-management-contract" and i.get("mapped") for i in intents.get("intents", [])),
        "GPAR-03": any(i.get("id") == "rust-cli-tui-primary-ux-surface" for i in parity.get("intentional_divergence", [])),
        "GPAR-04": int(adapter_summary.get("non_rust_native", 0)) == 0,
        "GPAR-05": any(i.get("id") == "environment-lifecycle-contract" and i.get("mapped") for i in intents.get("intents", []))
        and any(i.get("id") == "tool-call-parser-contract" and i.get("mapped") for i in intents.get("intents", [])),
        "GPAR-06": parity_shared.issubset(shared_diff_items),
        "GPAR-07": bool(proof_truth["pass"])
        and int(queue_summary.get("total_commits", 0)) > 0
        and int(queue_summary.get("by_disposition", {}).get("pending", 0)) == 0
        and int(queue_summary.get("unowned_divergence_count", 0)) == 0,
        "GPAR-08": int(divergence_summary.get("errors", 0)) == 0
        and int(divergence_summary.get("unowned", 0)) == 0
        and int(divergence_summary.get("review_overdue", 0)) == 0,
        "GPAR-09": bool(test_coverage_gate.get("pass", False)),
        "GPAR-10": bool(behavioral_gate.get("pass", False)),
        "GPAR-11": bool(deep_gate.get("pass", False)),
    }

    drift_observations = {
        "commits_behind": float(parity_summary.get("commits_behind", 0)),
        "upstream_patch_missing": float(parity_summary.get("upstream_patch_missing", 0)),
    }
    metrics = {
        "max_files_only_upstream": float(parity_summary.get("files_only_upstream", 0)),
        "max_unowned_divergences": float(divergence_summary.get("unowned", 0)),
        "max_divergence_review_overdue": float(divergence_summary.get("review_overdue", 0)),
        "min_test_intent_mapping_ratio": float(intent_summary.get("mapping_ratio", 0.0)),
        "min_test_coverage_tracked_behavior_ratio": float(
            test_coverage_summary.get("tracked_behavior_coverage_ratio", 0.0)
        ),
        "max_test_coverage_audit_critical_gaps": float(
            test_coverage_gate.get("critical_gaps", 0)
        ),
        "max_test_coverage_missing_rust_refs": float(
            test_coverage_summary.get("missing_rust_test_refs", 0)
        ),
        "min_fixture_reference_category_coverage_ratio": float(
            sota_harness_summary.get("domain_coverage_ratio", 0.0)
        ),
        "max_fixture_reference_critical_gaps": float(
            sota_harness_gate.get("critical_gaps", 0)
        ),
        "max_fixture_reference_missing_rust_refs": float(
            sota_harness_gate.get("missing_rust_test_refs", 0)
        ),
        "min_behavioral_similarity_ratio": float(
            behavioral_summary.get("behavioral_similarity_ratio", 0.0)
        ),
        "min_behavioral_superiority_cases": float(
            behavioral_summary.get("superior_cases", 0)
        ),
        "max_behavioral_regressions": float(
            behavioral_summary.get("regressions", 0)
        ),
        "max_behavioral_gaps": float(
            behavioral_summary.get("gaps", 0)
        ),
        "max_behavioral_unverified_cases": float(
            behavioral_summary.get("unverified_cases", 0)
        ),
        "max_behavioral_missing_rust_refs": float(
            behavioral_summary.get("missing_rust_test_refs", 0)
        ),
        "min_deep_problem_solving_ratio": float(
            deep_summary.get("deep_problem_solving_ratio", 0.0)
        ),
        "min_deep_problem_solving_superiority_cases": float(
            deep_summary.get("superior_cases", 0)
        ),
        "min_deep_problem_solving_total_cases": float(
            deep_summary.get("total_cases", 0)
        ),
        "max_deep_problem_solving_regressions": float(
            deep_summary.get("regressions", 0)
        ),
        "max_deep_problem_solving_gaps": float(
            deep_summary.get("gaps", 0)
        ),
        "max_deep_problem_solving_unverified_cases": float(
            deep_summary.get("unverified_cases", 0)
        ),
        "max_deep_problem_solving_missing_rust_refs": float(
            deep_summary.get("missing_rust_test_refs", 0)
        ),
        "max_queue_pending_commits": float(
            queue_summary.get("by_disposition", {}).get("pending", 0)
        ),
    }

    ci_gate = evaluate_gate(metrics, thresholds.get("ci_thresholds", {}))
    release_gate = evaluate_gate(metrics, thresholds.get("release_thresholds", {}))
    ci_gate["mode"] = gate_mode(thresholds.get("ci_thresholds", {}))
    release_gate["mode"] = gate_mode(thresholds.get("release_thresholds", {}))

    required = thresholds.get("release_thresholds", {}).get("required_workstreams_complete", [])
    required_ok = all(bool(gpar_completion.get(ws_id, False)) for ws_id in required)
    if not required_ok:
        release_gate["pass"] = False
        release_gate["checks"].append(
            {
                "metric": "required_workstreams_complete",
                "status": "fail",
                "actual": {k: bool(gpar_completion.get(k, False)) for k in required},
                "limit": "all true",
            }
        )
    else:
        release_gate["checks"].append(
            {
                "metric": "required_workstreams_complete",
                "status": "pass",
                "actual": {k: bool(gpar_completion.get(k, False)) for k in required},
                "limit": "all true",
            }
        )

    apply_ci_special_rules(
        metrics=metrics,
        ci_gate=ci_gate,
        release_gate=release_gate,
        ci_thresholds=thresholds.get("ci_thresholds", {}),
    )
    append_truth_failure(ci_gate, "source_provenance_and_required_metrics", proof_truth_errors)
    append_truth_failure(
        release_gate,
        "source_provenance_and_required_metrics",
        proof_truth_errors,
    )

    payload = {
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "program": thresholds.get("program", {}),
        "metrics": metrics,
        "drift_observations": drift_observations,
        "gpar_completion": gpar_completion,
        "ws_legacy_states": ws_states,
        "ci_gate": ci_gate,
        "release_gate": release_gate,
        "sources": {
            name: public_repo_relative_path(repo_root, path)
            for name, path in {
                "thresholds": args.thresholds,
                "parity_matrix": args.parity_matrix,
                "workstream_status": args.workstream_status,
                "intent_mapping": args.intent_mapping,
                "adapter_matrix": args.adapter_matrix,
                "shared_diff_classification": args.shared_diff_classification,
                "divergence_validation": args.divergence_validation,
                "patch_queue": args.patch_queue,
                "test_coverage_audit": args.test_coverage_audit,
                "fixture_reference_matrix": args.sota_harness_matrix,
                "behavioral_diff": args.behavioral_diff,
                "deep_problem_solving_diff": args.deep_problem_solving_diff,
                "source_ledger": args.source_ledger,
            }.items()
        },
        "truth": proof_truth,
        "queue_summary": queue_summary,
        "test_coverage_audit_summary": {
            "summary": test_coverage_summary,
            "audit_gate": test_coverage_gate,
        },
        "fixture_reference_coverage": {
            "authority": fixture_reference_authority,
            "scope": fixture_reference_scope,
            "summary": sota_harness_summary,
            "gate": sota_harness_gate,
        },
        "behavioral_diff_summary": {
            "summary": behavioral_summary,
            "gate": behavioral_gate,
        },
        "deep_problem_solving_summary": {
            "summary": deep_summary,
            "gate": deep_gate,
        },
    }

    out_json = (repo_root / args.out_json).resolve()
    out_md = (repo_root / args.out_md).resolve()

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    md = [
        "# Global Parity Gate Report",
        "",
        f"Generated: `{payload['generated_at_utc']}`",
        "",
        "## Gate Status",
        "",
        f"- CI gate: **{'PASS' if ci_gate['pass'] else 'FAIL'}**",
        f"- CI gate mode: `{ci_gate.get('mode', 'legacy')}`",
        f"- Release gate: **{'PASS' if release_gate['pass'] else 'FAIL'}**",
        f"- Release gate mode: `{release_gate.get('mode', 'legacy')}`",
        "",
        "## Source Truth",
        "",
        f"- Source truth: **{'PASS' if proof_truth['pass'] else 'FAIL'}**",
        f"- Provenance or required-metric errors: `{len(proof_truth_errors)}`",
        "",
        "## Metrics",
        "",
        "| Metric | Value |",
        "| --- | ---: |",
    ]
    for key, value in metrics.items():
        md.append(f"| `{key}` | {value} |")
    md.append("")
    md.append("## Drift Observations (Non-Gating)")
    md.append("")
    md.append("- Commit reachability counts are not parity evidence.")
    for key, value in drift_observations.items():
        md.append(f"- `{key}`: `{value}`")
    md.append("")
    md.append("## GPAR Ticket Completion")
    md.append("")
    md.append("| Ticket | Complete |")
    md.append("| --- | --- |")
    for key in sorted(gpar_completion.keys()):
        md.append(f"| `{key}` | {'yes' if gpar_completion[key] else 'no'} |")
    md.append("")

    md.append("## Test Coverage Audit")
    md.append("")
    md.append(f"- Audit gate: **{'PASS' if test_coverage_gate.get('pass') else 'FAIL'}**")
    md.append(
        "- Tracked behavior coverage ratio: "
        f"`{test_coverage_summary.get('tracked_behavior_coverage_ratio', 0)}`"
    )
    md.append(f"- Critical gaps: `{test_coverage_gate.get('critical_gaps', 0)}`")
    md.append(
        "- Missing Rust test refs: "
        f"`{test_coverage_summary.get('missing_rust_test_refs', 0)}`"
    )
    md.append("")

    md.append("## Fixture-Reference Regression Hygiene (Non-Authoritative)")
    md.append("")
    md.append(
        "- Coverage scope: "
        f"`{fixture_reference_scope.get('coverage_kind', 'missing')}`"
    )
    md.append(
        "- Capability-promotion authority: "
        f"`{str(fixture_reference_authority.get('capability_promotion_authoritative', 'missing')).lower()}`"
    )
    md.append(
        "- Production complete: "
        f"`{str(fixture_reference_authority.get('production_complete', 'missing')).lower()}`"
    )
    md.append(
        "- Fixture-reference matrix metrics below are regression hygiene only and do not establish production completeness."
    )
    md.append(
        f"- Fixture/reference gate: **{'PASS' if sota_harness_gate.get('pass') else 'FAIL'}**"
    )
    md.append(
        "- Fixture-category coverage ratio: "
        f"`{sota_harness_summary.get('domain_coverage_ratio', 0)}`"
    )
    md.append(
        "- Unresolved fixture-reference gaps: "
        f"`{sota_harness_gate.get('critical_gaps', 0)}`"
    )
    md.append(
        "- Missing referenced Rust test refs: "
        f"`{sota_harness_gate.get('missing_rust_test_refs', 0)}`"
    )
    md.append(
        "- Referenced Rust tests: "
        f"`{sota_harness_summary.get('direct_rust_tests', 0)}`"
    )
    md.append("")

    md.append("## Behavioral Similarity Diff")
    md.append("")
    md.append(f"- Behavioral gate: **{'PASS' if behavioral_gate.get('pass') else 'FAIL'}**")
    md.append(
        "- Similarity ratio: "
        f"`{behavioral_summary.get('behavioral_similarity_ratio', 0)}`"
    )
    md.append(
        "- Superior cases: "
        f"`{behavioral_summary.get('superior_cases', 0)}`"
    )
    md.append(
        "- Regressions: "
        f"`{behavioral_summary.get('regressions', 0)}`"
    )
    md.append(
        "- Gaps: "
        f"`{behavioral_summary.get('gaps', 0)}`"
    )
    md.append(
        "- Unverified cases: "
        f"`{behavioral_summary.get('unverified_cases', 0)}`"
    )
    md.append(
        "- Missing Rust test refs: "
        f"`{behavioral_summary.get('missing_rust_test_refs', 0)}`"
    )
    md.append("")

    md.append("## Deep Problem-Solving Diff")
    md.append("")
    md.append(f"- Deep problem-solving gate: **{'PASS' if deep_gate.get('pass') else 'FAIL'}**")
    md.append(
        "- Deep problem-solving ratio: "
        f"`{deep_summary.get('deep_problem_solving_ratio', 0)}`"
    )
    md.append(
        "- Superior cases: "
        f"`{deep_summary.get('superior_cases', 0)}`"
    )
    md.append(
        "- Regressions: "
        f"`{deep_summary.get('regressions', 0)}`"
    )
    md.append(
        "- Gaps: "
        f"`{deep_summary.get('gaps', 0)}`"
    )
    md.append(
        "- Unverified cases: "
        f"`{deep_summary.get('unverified_cases', 0)}`"
    )
    md.append(
        "- Missing Rust test refs: "
        f"`{deep_summary.get('missing_rust_test_refs', 0)}`"
    )
    md.append("")

    md.append("## Queue Summary")
    md.append("")
    md.append(
        f"- Upstream missing commits tracked: `{queue_summary.get('total_commits', 0)}`."
    )
    by_ticket = queue_summary.get("by_target_ticket", {})
    if isinstance(by_ticket, dict):
        md.append("- By target ticket:")
        for ticket, count in sorted(by_ticket.items(), key=lambda kv: kv[0]):
            md.append(f"  - `#{ticket}`: `{count}`")
    md.append("")
    out_md.write_text("\n".join(md) + "\n", encoding="utf-8")

    print(f"Wrote {out_json}")
    print(f"Wrote {out_md}")

    if args.check_release and not release_gate["pass"]:
        return 1
    if args.check_ci and not ci_gate["pass"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
