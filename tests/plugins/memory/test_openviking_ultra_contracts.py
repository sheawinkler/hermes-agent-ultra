import importlib.util
import json
import sys
import types
import zipfile
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[3]
OPENVIKING_MODULE = REPO_ROOT / "plugins" / "memory" / "openviking" / "__init__.py"


def load_openviking_module(monkeypatch):
    """Load the provider file directly; this checkout has pyc-only support modules."""
    agent_pkg = types.ModuleType("agent")
    agent_pkg.__path__ = []
    memory_provider = types.ModuleType("agent.memory_provider")

    class MemoryProvider:
        pass

    memory_provider.MemoryProvider = MemoryProvider

    tools_pkg = types.ModuleType("tools")
    tools_pkg.__path__ = []
    registry = types.ModuleType("tools.registry")
    registry.tool_error = lambda message: json.dumps({"error": message})

    monkeypatch.setitem(sys.modules, "agent", agent_pkg)
    monkeypatch.setitem(sys.modules, "agent.memory_provider", memory_provider)
    monkeypatch.setitem(sys.modules, "tools", tools_pkg)
    monkeypatch.setitem(sys.modules, "tools.registry", registry)

    module_name = "_openviking_contract_test"
    spec = importlib.util.spec_from_file_location(module_name, OPENVIKING_MODULE)
    module = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, module_name, module)
    assert spec and spec.loader
    spec.loader.exec_module(module)
    return module


class FakeClient:
    def __init__(self, response=None):
        self.response = response or {"result": {"written_bytes": 0}}
        self.posts = []

    def post(self, path, payload=None, **kwargs):
        self.posts.append((path, payload or {}, kwargs))
        return self.response


def test_viking_remember_writes_direct_content_file(monkeypatch):
    module = load_openviking_module(monkeypatch)
    monkeypatch.setattr(
        module.uuid,
        "uuid4",
        lambda: types.SimpleNamespace(hex="abc123def4567890"),
    )

    provider = module.OpenVikingMemoryProvider()
    provider._user = "alice"
    provider._client = FakeClient({"result": {"written_bytes": 27}})

    result = json.loads(
        provider._tool_remember({
            "content": "Prefers terse status updates.",
            "category": "preference",
        })
    )

    assert result == {
        "status": "stored",
        "message": "Memory stored (27b) and queued for vector indexing.",
    }
    assert provider._client.posts == [(
        "/api/v1/content/write",
        {
            "uri": "viking://user/alice/memories/preferences/mem_abc123def456.md",
            "content": "Prefers terse status updates.",
            "mode": "create",
        },
        {},
    )]


def test_builtin_memory_write_mirrors_to_target_subdirectory(monkeypatch):
    module = load_openviking_module(monkeypatch)
    monkeypatch.setattr(
        module.uuid,
        "uuid4",
        lambda: types.SimpleNamespace(hex="fedcba9876543210"),
    )
    fake_client = FakeClient()
    monkeypatch.setattr(module, "_VikingClient", lambda *args, **kwargs: fake_client)

    class ImmediateThread:
        def __init__(self, target, **kwargs):
            self._target = target

        def start(self):
            self._target()

    monkeypatch.setattr(module.threading, "Thread", ImmediateThread)

    provider = module.OpenVikingMemoryProvider()
    provider._client = object()
    provider._endpoint = "http://openviking.test"
    provider._api_key = "test-key"
    provider._account = "default"
    provider._user = "alice"
    provider._agent = "hermes"

    provider.on_memory_write(
        "add",
        "memory",
        "A reusable troubleshooting pattern.",
        metadata={"source": "test"},
    )

    assert fake_client.posts == [(
        "/api/v1/content/write",
        {
            "uri": "viking://user/alice/memories/patterns/mem_fedcba987654.md",
            "content": "A reusable troubleshooting pattern.",
            "mode": "create",
        },
        {},
    )]


def test_zip_directory_skips_symlink_escape(monkeypatch, tmp_path):
    module = load_openviking_module(monkeypatch)
    root = tmp_path / "root"
    root.mkdir()
    (root / "safe.txt").write_text("safe", encoding="utf-8")
    outside = tmp_path / "outside.txt"
    outside.write_text("outside", encoding="utf-8")

    try:
        (root / "escape.txt").symlink_to(outside)
    except OSError as exc:
        pytest.skip(f"symlink creation unavailable: {exc}")

    zip_path = module._zip_directory(root)
    try:
        with zipfile.ZipFile(zip_path) as zip_file:
            assert zip_file.namelist() == ["safe.txt"]
            assert zip_file.read("safe.txt") == b"safe"
    finally:
        zip_path.unlink(missing_ok=True)
