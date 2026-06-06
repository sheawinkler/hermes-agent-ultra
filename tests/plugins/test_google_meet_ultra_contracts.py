"""Ultra-specific Google Meet parity contracts."""

from __future__ import annotations

import asyncio
import importlib
import json
import sys


def test_google_meet_imports_without_upstream_hermes_constants(tmp_path, monkeypatch) -> None:
    """Google Meet keeps its own home helper in the Rust-first tree."""
    hermes_home = tmp_path / ".hermes-ultra"
    monkeypatch.setenv("HERMES_HOME", str(hermes_home))

    for name in list(sys.modules):
        if name == "plugins.google_meet" or name.startswith("plugins.google_meet."):
            monkeypatch.delitem(sys.modules, name, raising=False)
    monkeypatch.setitem(sys.modules, "hermes_constants", None)

    for module_name in (
        "plugins.google_meet.cli",
        "plugins.google_meet.process_manager",
        "plugins.google_meet.node.registry",
        "plugins.google_meet.node.server",
    ):
        importlib.import_module(module_name)

    helper = importlib.import_module("plugins.google_meet._hermes_home")
    assert helper.get_hermes_home() == hermes_home


def test_google_meet_node_say_requires_text_before_active_meeting(tmp_path) -> None:
    from plugins.google_meet.node import protocol
    from plugins.google_meet.node.server import NodeServer

    server = NodeServer(token_path=tmp_path / "node_token.json")
    token = server.ensure_token()
    req = protocol.make_request("say", token, {"text": "   "})

    resp = asyncio.run(server._handle_request(req))

    assert resp["type"] == "response"
    assert resp["payload"] == {
        "ok": False,
        "enqueued": False,
        "reason": "text is required",
    }


def test_google_meet_node_say_writes_queue_only_for_realtime(tmp_path, monkeypatch) -> None:
    from plugins.google_meet import process_manager as pm
    from plugins.google_meet.node import protocol
    from plugins.google_meet.node.server import NodeServer

    out_dir = tmp_path / "meet-out"
    out_dir.mkdir()
    monkeypatch.setattr(
        pm,
        "_read_active",
        lambda: {
            "pid": 123,
            "meeting_id": "abc-defg-hij",
            "mode": "realtime",
            "out_dir": str(out_dir),
        },
    )

    server = NodeServer(token_path=tmp_path / "node_token.json")
    token = server.ensure_token()
    req = protocol.make_request("say", token, {"text": "hello"})

    resp = asyncio.run(server._handle_request(req))

    assert resp["payload"]["ok"] is True
    assert resp["payload"]["enqueued"] is True
    lines = (out_dir / "say_queue.jsonl").read_text(encoding="utf-8").splitlines()
    assert json.loads(lines[0])["text"] == "hello"


def test_google_meet_speaker_filter_contract() -> None:
    from plugins.google_meet.meet_bot import _looks_like_human_speaker

    assert not _looks_like_human_speaker("", "Hermes Agent")
    assert not _looks_like_human_speaker("Unknown", "Hermes Agent")
    assert not _looks_like_human_speaker("You", "Hermes Agent")
    assert not _looks_like_human_speaker("Hermes Agent", "Hermes Agent")
    assert _looks_like_human_speaker("Alice", "Hermes Agent")
