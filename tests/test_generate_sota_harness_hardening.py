from __future__ import annotations

import importlib.util
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-sota-harness-hardening.py"


def _module():
    spec = importlib.util.spec_from_file_location("generate_sota_harness_hardening", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_replay_index_serializes_non_authoritative_fixture_scope() -> None:
    module = _module()
    matrix = {
        "gate": {"pass": True},
        "domains": [
            {
                "id": "fixture-domain",
                "title": "Fixture domain",
                "status": "fixture_reference_regression_coverage",
                "why": "Fixture-only regression coverage.",
                "fixtures": ["fixtures/example.json"],
                "rust_tests": [{"file": "src/example.rs", "name": "fixture_regression"}],
                "fixture_reference_categories": ["example fixture category"],
            }
        ],
    }

    replay = module.build_replay_index(
        {"audit_gate": {"pass": True}},
        {"release_gate": {"pass": True}, "ci_gate": {"pass": True}},
        {"summary": {"by_disposition": {"pending": 0}}},
        matrix,
    )

    assert replay["authority"] == {
        "capability_promotion_authoritative": False,
        "production_complete": False,
    }
    assert replay["scope"]["coverage_kind"] == "fixture_reference_regression"
    assert replay["latest_gate_state"] == {
        "release_gate_pass": True,
        "ci_gate_pass": True,
        "test_coverage_gate_pass": True,
        "fixture_reference_gate_pass": True,
        "production_complete": False,
        "queue_pending": 0,
    }
    group = replay["replay_evidence_groups"][0]
    assert group["fixture_reference_categories"] == ["example fixture category"]
    assert "required_capabilities" not in group

    markdown = module.render_replay_md(replay)
    assert "Fixture-Reference Evidence Index" in markdown
    assert "Capability-promotion authority: `false`" in markdown
    assert "Production complete: `false`" in markdown


def test_hardening_budget_uses_fixture_reference_hygiene_labels() -> None:
    module = _module()
    snapshot = {
        "queue_pending": 0,
        "tracked_behavior_coverage_ratio": 1.0,
        "fixture_reference_category_coverage_ratio": 1.0,
        "rust_test_functions": 1,
        "coverage_manifest_entries": 1,
        "fixture_reference_workflow_replay_steps": 1,
        "fixture_reference_protocol_cases": 1,
        "fixture_reference_fault_scenarios": 1,
    }

    budget = module.build_budget(snapshot, {"release_thresholds": {"functional_parity": {}}})

    assert budget["authority"]["capability_promotion_authoritative"] is False
    assert budget["authority"]["production_complete"] is False
    assert budget["scope"]["coverage_kind"] == "fixture_reference_regression"
    assert all(item["pass"] for item in budget["budgets"])
    assert {
        item["id"] for item in budget["budgets"]
    } >= {
        "fixture_reference_category_coverage_ratio",
        "fixture_reference_workflow_replay_steps",
        "fixture_reference_protocol_cases",
        "fixture_reference_fault_scenarios",
    }
    assert "Fixture-Reference Harness Hygiene Budget" in module.render_budget_md(budget)
