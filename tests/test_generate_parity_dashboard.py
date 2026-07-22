from __future__ import annotations

import importlib.util
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "generate-parity-dashboard.py"


def _module():
    spec = importlib.util.spec_from_file_location("generate_parity_dashboard", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_dashboard_labels_fixture_reference_matrix_non_authoritative() -> None:
    module = _module()
    rendered = module.render_dashboard(
        {},
        {},
        {},
        {"release_gate": {"pass": True}, "ci_gate": {"pass": True}},
        {"summary": {}, "audit_gate": {"pass": True}},
        {
            "authority": {
                "capability_promotion_authoritative": False,
                "production_complete": False,
            },
            "scope": {"coverage_kind": "fixture_reference_regression"},
            "summary": {"domains_total": 1, "domains_passing": 1},
            "gate": {"pass": True, "critical_gaps": 0, "missing_rust_test_refs": 0},
        },
        {"summary": {}, "gate": {"pass": True}},
        {"summary": {}, "gate": {"pass": True}},
    )

    assert "Fixture-Reference Regression Hygiene (Non-Authoritative)" in rendered
    assert "Capability-promotion authority: `false`" in rendered
    assert "Production complete: `false`" in rendered
    assert "do not establish an implemented product capability" in rendered
