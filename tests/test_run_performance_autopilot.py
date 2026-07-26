from __future__ import annotations

import importlib.util
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "run-performance-autopilot.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("performance_autopilot", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_policy_pack_retrieval_is_parsed_as_grounded_health() -> None:
    module = _load_module()
    payload = {
        "ok": True,
        "retrieval_mode": "balanced",
        "retrieval": {
            "degraded": False,
            "warnings": ["one slow source"],
            "top_results": [
                {"source": "topic_rollups"},
                {"source": "postgres_pgvector"},
                {"source": "topic_rollups"},
            ],
        },
    }

    summary = module.contextlattice_summary(payload)

    assert summary == {
        "healthy": True,
        "warnings": 1,
        "python_fallbacks": 0,
        "route_owner_class": "",
        "retrieval_mode": "balanced",
        "source_counts": {"topic_rollups": 2, "postgres_pgvector": 1},
        "queue_pending_total": None,
    }


def test_policy_pack_mode_survives_without_retrieval_debug() -> None:
    module = _load_module()

    summary = module.contextlattice_summary({"ok": True, "retrieval_mode": "balanced"})

    assert summary["healthy"] is True
    assert summary["retrieval_mode"] == "balanced"
    assert summary["source_counts"] is None


def test_degraded_empty_retrieval_cannot_be_reported_stable() -> None:
    module = _load_module()
    payload = {
        "ok": True,
        "retrieval_mode": "balanced",
        "retrieval": {
            "degraded": True,
            "warnings": ["all primary sources unavailable"],
            "top_results": [],
        },
    }
    context_gate = {
        "ok": True,
        "stdout_tail": json.dumps(payload),
        "stderr_tail": "",
    }
    passing = {"ok": True, "stdout_tail": "", "stderr_tail": ""}

    recommendations = module.build_recommendations(
        passing,
        passing,
        passing,
        context_gate,
    )
    ids = {entry["id"] for entry in recommendations}

    assert "CONTEXTLATTICE_UNHEALTHY" in ids
    assert "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE" in ids
    assert "PERF_STABLE" not in ids
