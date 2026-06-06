"""WS3 example dashboard plugin divergence contracts."""

from __future__ import annotations

import asyncio
import importlib.util
import json
import sys
import types
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
PLUGIN_ROOT = REPO_ROOT / "plugins" / "example-dashboard" / "dashboard"


def test_example_dashboard_manifest_matches_local_slot_demo() -> None:
    manifest = json.loads((PLUGIN_ROOT / "manifest.json").read_text())
    bundle = (PLUGIN_ROOT / "dist" / "index.js").read_text()

    assert manifest["description"] == "Example dashboard plugin — demonstrates the plugin SDK"
    assert manifest["slots"] == ["sessions:top"]
    assert manifest["entry"] == "dist/index.js"
    assert 'registerSlot("example", "sessions:top", SessionsTopBanner)' in bundle
    assert "window.__HERMES_PLUGIN_SDK__" in bundle


def test_example_dashboard_hello_route_remains_stable(monkeypatch) -> None:
    class APIRouter:
        def __init__(self) -> None:
            self.routes = []

        def get(self, path):
            def decorator(func):
                self.routes.append((path, func))
                return func

            return decorator

    fastapi = types.ModuleType("fastapi")
    fastapi.APIRouter = APIRouter
    monkeypatch.setitem(sys.modules, "fastapi", fastapi)

    spec = importlib.util.spec_from_file_location(
        "_ws3_example_dashboard_api",
        PLUGIN_ROOT / "plugin_api.py",
    )
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, "_ws3_example_dashboard_api", module)
    spec.loader.exec_module(module)

    assert module.router.routes[0][0] == "/hello"
    assert asyncio.run(module.hello()) == {
        "message": "Hello from the example plugin!",
        "plugin": "example",
        "version": "1.0.0",
    }
