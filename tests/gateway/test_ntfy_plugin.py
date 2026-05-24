"""Regression coverage for the ntfy platform plugin port."""

from __future__ import annotations

import asyncio
import sys
import types
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock

from tests.gateway._plugin_adapter_loader import load_plugin_adapter


class Platform(str):
    _cache: dict[str, "Platform"] = {}

    def __new__(cls, value: str):
        if value in cls._cache:
            return cls._cache[value]
        obj = str.__new__(cls, value)
        cls._cache[value] = obj
        return obj

    @property
    def value(self) -> str:
        return str(self)


@dataclass
class PlatformConfig:
    enabled: bool = True
    extra: dict | None = None


class MessageType:
    TEXT = "text"


@dataclass
class MessageEvent:
    text: str
    message_type: str
    source: dict
    message_id: str
    raw_message: dict
    timestamp: object


@dataclass
class SendResult:
    success: bool
    message_id: str | None = None
    error: str | None = None


class BasePlatformAdapter:
    def __init__(self, *, config: PlatformConfig, platform: Platform):
        self.config = config
        self.platform = platform
        self.name = platform.value
        self._running = False

    def _mark_connected(self) -> None:
        self._running = True

    def _mark_disconnected(self) -> None:
        self._running = False

    def _set_fatal_error(self, *_args, **_kwargs) -> None:
        pass

    def build_source(self, **kwargs) -> dict:
        return kwargs

    async def handle_message(self, event: MessageEvent) -> None:
        self.last_event = event


def _install_gateway_stubs() -> None:
    gateway_pkg = sys.modules.setdefault("gateway", types.ModuleType("gateway"))
    gateway_pkg.__path__ = [str(Path(__file__).resolve().parents[2] / "gateway")]

    config_mod = types.ModuleType("gateway.config")
    config_mod.Platform = Platform
    config_mod.PlatformConfig = PlatformConfig
    sys.modules["gateway.config"] = config_mod

    platforms_pkg = sys.modules.setdefault("gateway.platforms", types.ModuleType("gateway.platforms"))
    platforms_pkg.__path__ = []
    base_mod = types.ModuleType("gateway.platforms.base")
    base_mod.BasePlatformAdapter = BasePlatformAdapter
    base_mod.MessageEvent = MessageEvent
    base_mod.MessageType = MessageType
    base_mod.SendResult = SendResult
    sys.modules["gateway.platforms.base"] = base_mod


_install_gateway_stubs()
_ntfy = load_plugin_adapter("ntfy")


def _run(coro):
    return asyncio.get_event_loop().run_until_complete(coro)


def test_auth_header_strips_token_and_supports_basic_auth():
    assert _ntfy._build_auth_header(" token\n") == {"Authorization": "Bearer token"}
    assert _ntfy._build_auth_header("user:pass")["Authorization"].startswith("Basic ")
    assert _ntfy._build_auth_header(" \n") == {}


def test_env_enablement_seeds_topic_publish_auth_markdown_and_home(monkeypatch):
    monkeypatch.setenv("NTFY_TOPIC", "inbox")
    monkeypatch.setenv("NTFY_SERVER_URL", "https://ntfy.example/")
    monkeypatch.setenv("NTFY_PUBLISH_TOPIC", "outbox")
    monkeypatch.setenv("NTFY_TOKEN", "secret")
    monkeypatch.setenv("NTFY_MARKDOWN", "yes")
    monkeypatch.setenv("NTFY_HOME_CHANNEL", "home")
    monkeypatch.setenv("NTFY_HOME_CHANNEL_NAME", "Home")

    seed = _ntfy._env_enablement()

    assert seed == {
        "topic": "inbox",
        "server": "https://ntfy.example",
        "publish_topic": "outbox",
        "token": "secret",
        "markdown": True,
        "home_channel": {"chat_id": "home", "name": "Home"},
    }


def test_on_message_uses_topic_identity_not_spoofable_title():
    adapter = _ntfy.NtfyAdapter(PlatformConfig(extra={"topic": "trusted-topic"}))
    adapter.handle_message = AsyncMock()

    _run(adapter._on_message({
        "id": "msg-1",
        "topic": "trusted-topic",
        "title": "admin",
        "message": "hello",
        "time": 1710000000,
    }))

    event = adapter.handle_message.await_args.args[0]
    assert event.source["user_id"] == "trusted-topic"
    assert event.source["user_name"] == "trusted-topic"
    assert event.source["user_id"] != "admin"


def test_standalone_send_truncates_and_uses_shared_auth(monkeypatch):
    posted: dict = {}

    class FakeResponse:
        status_code = 200
        text = "ok"

        def json(self):
            return {"id": "ntfy-id"}

    class FakeClient:
        def __init__(self, *args, **kwargs):
            pass

        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            return None

        async def post(self, url, *, content, headers):
            posted["url"] = url
            posted["content"] = content
            posted["headers"] = headers
            return FakeResponse()

    monkeypatch.setattr(_ntfy, "HTTPX_AVAILABLE", True)
    monkeypatch.setattr(_ntfy.httpx, "AsyncClient", FakeClient)

    result = _run(_ntfy._standalone_send(
        SimpleNamespace(extra={
            "server": "https://ntfy.example/",
            "topic": "default-topic",
            "token": " user:pass\n",
            "markdown": True,
        }),
        "",
        "x" * (_ntfy.MAX_MESSAGE_LENGTH + 20),
    ))

    assert result == {"success": True, "platform": "ntfy", "chat_id": "default-topic", "message_id": "ntfy-id"}
    assert posted["url"] == "https://ntfy.example/default-topic"
    assert len(posted["content"]) == _ntfy.MAX_MESSAGE_LENGTH
    assert posted["headers"]["Authorization"].startswith("Basic ")
    assert posted["headers"]["X-Markdown"] == "true"


def test_register_exposes_plugin_contract():
    calls = []
    ctx = SimpleNamespace(register_platform=lambda **kwargs: calls.append(kwargs))

    _ntfy.register(ctx)

    registration = calls[0]
    assert registration["name"] == "ntfy"
    assert registration["env_enablement_fn"] is _ntfy._env_enablement
    assert registration["standalone_sender_fn"] is _ntfy._standalone_send
    assert registration["cron_deliver_env_var"] == "NTFY_HOME_CHANNEL"
    assert registration["max_message_length"] == _ntfy.MAX_MESSAGE_LENGTH
    assert registration["pii_safe"] is True
