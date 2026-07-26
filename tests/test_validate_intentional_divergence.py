from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "scripts" / "validate-intentional-divergence.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("validate_intentional_divergence", SCRIPT)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)  # type: ignore[assignment]
    return module


def test_report_serializes_divergence_file_relative_to_repo(
    tmp_path: Path, monkeypatch
) -> None:
    module = _load_module()
    divergence_file = tmp_path / "docs" / "parity" / "intentional-divergence.json"
    divergence_file.parent.mkdir(parents=True)
    divergence_file.write_text('{"items": []}\n', encoding="utf-8")

    monkeypatch.setattr(module, "ensure_remote", lambda *_args: None)
    monkeypatch.setattr(module, "ls_tree_paths", lambda *_args: set())
    monkeypatch.setattr(
        sys,
        "argv",
        [str(SCRIPT), "--repo-root", str(tmp_path), "--no-fetch"],
    )

    assert module.main() == 0
    report = json.loads(
        (tmp_path / "docs" / "parity" / "divergence-validation.json").read_text(
            encoding="utf-8"
        )
    )
    assert report["divergence_file"] == "docs/parity/intentional-divergence.json"
    assert str(tmp_path) not in json.dumps(report)
