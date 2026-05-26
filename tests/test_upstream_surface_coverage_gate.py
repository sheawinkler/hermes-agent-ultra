from __future__ import annotations

import importlib.util
from pathlib import Path


def _load_gate_module():
    repo_root = Path(__file__).resolve().parents[1]
    script_path = repo_root / "scripts" / "run-upstream-surface-coverage-gate.py"
    spec = importlib.util.spec_from_file_location("surface_gate", script_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_build_report_flags_missing(monkeypatch):
    gate = _load_gate_module()
    repo_root = Path(".").resolve()
    prefixes = ["skills", "website"]

    upstream_map = {
        "skills": ["skills/a.md", "skills/b.md"],
        "website": ["website/docs/one.md"],
    }
    present = {"skills/a.md", "website/docs/one.md"}

    monkeypatch.setattr(gate, "list_files", lambda _root, _ref, p: upstream_map[p])
    monkeypatch.setattr(gate, "ref_has_path", lambda _root, _ref, path: path in present)

    report = gate.build_report(repo_root, "HEAD", "ref", "upstream/main", prefixes, [])
    assert report["ok"] is False
    assert report["summary"]["missing_total"] == 1
    assert report["missing_paths"] == ["skills/b.md"]
    assert report["by_prefix"]["skills"]["coverage_ratio"] == 0.5


def test_build_report_passes_when_all_present(monkeypatch):
    gate = _load_gate_module()
    repo_root = Path(".").resolve()
    prefixes = ["plugins"]

    monkeypatch.setattr(
        gate,
        "list_files",
        lambda _root, _ref, _prefix: ["plugins/a.py", "plugins/b.py"],
    )
    monkeypatch.setattr(gate, "ref_has_path", lambda _root, _ref, _path: True)

    report = gate.build_report(repo_root, "HEAD", "ref", "upstream/main", prefixes, [])
    assert report["ok"] is True
    assert report["summary"]["missing_total"] == 0
    assert report["summary"]["present_locally"] == 2


def test_rust_only_prefix_violations_blocks_test_prefixes():
    gate = _load_gate_module()
    violations = gate.rust_only_prefix_violations(
        ["skills", "tests", "tests/cli", "./test/helpers", "docs"]
    )
    assert violations == ["./test/helpers", "tests", "tests/cli"]


def test_rust_only_prefix_violations_allows_core_prefixes():
    gate = _load_gate_module()
    violations = gate.rust_only_prefix_violations(
        ["skills", "optional-skills", "plugins", "website", "ui-tui", "docs"]
    )
    assert violations == []
