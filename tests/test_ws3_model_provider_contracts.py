"""Contracts for WS3 model-provider parity deltas."""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent


class _ProviderProfile:
    def __init__(self, **kwargs):
        self.__dict__.update(kwargs)

    def build_api_kwargs_extras(self, **_context):
        return {}, {}


def _load_provider(path: str, module_name: str):
    registered = []
    providers_module = types.ModuleType("providers")
    providers_base_module = types.ModuleType("providers.base")
    providers_module.register_provider = registered.append
    providers_base_module.ProviderProfile = _ProviderProfile
    providers_base_module.OMIT_TEMPERATURE = object()

    old_modules = {
        name: sys.modules.get(name)
        for name in ("providers", "providers.base")
    }
    sys.modules["providers"] = providers_module
    sys.modules["providers.base"] = providers_base_module
    try:
        spec = importlib.util.spec_from_file_location(module_name, REPO_ROOT / path)
        assert spec is not None
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
    finally:
        for name, previous in old_modules.items():
            if previous is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous
    return module, registered


def _read(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def test_deepseek_profile_has_thinking_controls() -> None:
    _module, registered = _load_provider(
        "plugins/model-providers/deepseek/__init__.py",
        "_ws3_deepseek_provider",
    )
    (profile,) = registered

    extra_body, top_level = profile.build_api_kwargs_extras(
        model="deepseek-v4-flash",
        reasoning_config={"enabled": True, "effort": "xhigh"},
    )
    assert extra_body == {"thinking": {"type": "enabled"}}
    assert top_level == {"reasoning_effort": "max"}

    extra_body, top_level = profile.build_api_kwargs_extras(
        model="deepseek-chat",
        reasoning_config={"enabled": True, "effort": "high"},
    )
    assert extra_body == {}
    assert top_level == {}

    extra_body, top_level = profile.build_api_kwargs_extras(
        model="deepseek-reasoner",
        reasoning_config={"enabled": False},
    )
    assert extra_body == {"thinking": {"type": "disabled"}}
    assert top_level == {}


def test_opencode_go_profile_has_model_specific_reasoning_controls() -> None:
    _module, registered = _load_provider(
        "plugins/model-providers/opencode-zen/__init__.py",
        "_ws3_opencode_provider",
    )
    profiles = {profile.name: profile for profile in registered}
    go = profiles["opencode-go"]

    extra_body, top_level = go.build_api_kwargs_extras(
        model="provider/kimi-k2-turbo-preview",
        reasoning_config={"enabled": True, "effort": "xhigh"},
    )
    assert extra_body == {"thinking": {"type": "enabled"}}
    assert top_level == {"reasoning_effort": "high"}

    extra_body, top_level = go.build_api_kwargs_extras(
        model="deepseek-v4-pro",
        reasoning_config={"enabled": True, "effort": "max"},
    )
    assert extra_body == {"thinking": {"type": "enabled"}}
    assert top_level == {"reasoning_effort": "max"}

    extra_body, top_level = go.build_api_kwargs_extras(
        model="glm-5",
        reasoning_config={"enabled": True, "effort": "high"},
    )
    assert extra_body == {}
    assert top_level == {}


def test_xiaomi_disables_dedicated_health_check() -> None:
    _module, registered = _load_provider(
        "plugins/model-providers/xiaomi/__init__.py",
        "_ws3_xiaomi_provider",
    )
    (profile,) = registered
    assert profile.supports_health_check is False


def test_openrouter_session_id_sticky_routing_matches_upstream_contract() -> None:
    _module, registered = _load_provider(
        "plugins/model-providers/openrouter/__init__.py",
        "_ws3_openrouter_provider",
    )
    (profile,) = registered

    body = profile.build_extra_body(session_id="sess-abc123")
    assert body == {"session_id": "sess-abc123"}

    body = profile.build_extra_body(
        session_id="sess-abc123",
        provider_preferences={"allow": ["anthropic"]},
    )
    assert body["session_id"] == "sess-abc123"
    assert body["provider"] == {"allow": ["anthropic"]}

    _extra_body, top_level = profile.build_api_kwargs_extras(
        model="x-ai/grok-4",
        session_id="sess-abc123",
    )
    assert top_level["extra_headers"]["x-grok-conv-id"] == "sess-abc123"


def test_local_model_provider_deltas_remain_exact_and_documented() -> None:
    azure_profile = _read("plugins/model-providers/azure-foundry/__init__.py")
    azure_manifest = _read("plugins/model-providers/azure-foundry/plugin.yaml")
    kimi_profile = _read("plugins/model-providers/kimi-coding/__init__.py")
    nous_profile = _read("plugins/model-providers/nous/__init__.py")

    assert "Azure AI Foundry provider profile" in azure_profile
    assert "description: Azure AI Foundry" in azure_manifest
    assert 'effort in ("low", "medium", "high")' in kimi_profile
    assert 'effort in {"low", "medium", "high"}' not in kimi_profile
    assert 'return {"tags": ["product=hermes-agent"]}' in nous_profile
    assert "agent.portal_tags" not in nous_profile
