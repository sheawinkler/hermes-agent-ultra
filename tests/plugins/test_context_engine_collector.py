"""Tests for context-engine plugin command forwarding."""

from __future__ import annotations

import logging
import sys
import types
from types import SimpleNamespace

from plugins.context_engine import _EngineCollector


def _install_hermes_cli_stubs(monkeypatch, *, resolve_command=None, manager=None):
    package = types.ModuleType("hermes_cli")
    commands_mod = types.ModuleType("hermes_cli.commands")
    plugins_mod = types.ModuleType("hermes_cli.plugins")
    commands_mod.resolve_command = resolve_command or (lambda _name: None)
    manager = manager or SimpleNamespace(_plugin_commands={})
    plugins_mod.get_plugin_manager = lambda: manager
    package.commands = commands_mod
    package.plugins = plugins_mod
    monkeypatch.setitem(sys.modules, "hermes_cli", package)
    monkeypatch.setitem(sys.modules, "hermes_cli.commands", commands_mod)
    monkeypatch.setitem(sys.modules, "hermes_cli.plugins", plugins_mod)
    return manager


def test_context_engine_register_command_forwards_to_plugin_registry(monkeypatch):
    manager = _install_hermes_cli_stubs(monkeypatch)
    handler = lambda args: f"ok:{args}"

    collector = _EngineCollector(engine_name="lcm")
    collector.register_command(
        "/LCM Status",
        handler,
        description="LCM status",
        args_hint="  <session>  ",
    )

    entry = manager._plugin_commands["lcm-status"]
    assert entry["handler"] is handler
    assert entry["description"] == "LCM status"
    assert entry["plugin"] == "context-engine:lcm"
    assert entry["args_hint"] == "<session>"
    assert collector._registered_commands == ["lcm-status"]


def test_context_engine_register_command_rejects_builtin_conflict(
    monkeypatch, caplog
):
    manager = _install_hermes_cli_stubs(
        monkeypatch,
        resolve_command=lambda name: object() if name == "help" else None,
    )
    collector = _EngineCollector(engine_name="lcm")

    with caplog.at_level(logging.WARNING, logger="plugins.context_engine"):
        collector.register_command("help", lambda _args: "bad")

    assert manager._plugin_commands == {}
    assert "conflicts with a built-in command" in caplog.text


def test_context_engine_register_command_does_not_clobber_plugin_command(
    monkeypatch, caplog
):
    existing_handler = lambda _args: "plugin"
    manager = SimpleNamespace(
        _plugin_commands={
            "lcm": {
                "handler": existing_handler,
                "description": "Plugin command",
                "plugin": "regular-plugin",
                "args_hint": "",
            }
        }
    )
    _install_hermes_cli_stubs(monkeypatch, manager=manager)
    collector = _EngineCollector(engine_name="lcm-engine")

    with caplog.at_level(logging.WARNING, logger="plugins.context_engine"):
        collector.register_command("/lcm", lambda _args: "engine")

    assert manager._plugin_commands["lcm"]["handler"] is existing_handler
    assert manager._plugin_commands["lcm"]["plugin"] == "regular-plugin"
    assert "already registered by a plugin" in caplog.text
