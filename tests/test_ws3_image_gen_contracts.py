"""WS3 image generation provider parity contracts.

The Rust checkout does not include the upstream Python ``agent``/``tools``
source modules these plugins import, so these tests load the plugin files with
small local stubs and assert the shared-diff behavior directly.
"""

from __future__ import annotations

import importlib.util
import sys
import types
from pathlib import Path
from types import SimpleNamespace

REPO_ROOT = Path(__file__).resolve().parents[1]


def _install_provider_stubs(monkeypatch, *, xai_creds: dict[str, str] | None = None) -> None:
    agent_mod = types.ModuleType("agent")
    image_provider_mod = types.ModuleType("agent.image_gen_provider")

    class ImageGenProvider:
        pass

    def error_response(**kwargs):
        return {"success": False, **kwargs}

    def success_response(*, image, model, prompt, aspect_ratio, provider, extra=None):
        result = {
            "success": True,
            "image": image,
            "model": model,
            "prompt": prompt,
            "aspect_ratio": aspect_ratio,
            "provider": provider,
        }
        result.update(extra or {})
        return result

    def resolve_aspect_ratio(value):
        return value if value in {"landscape", "portrait", "square"} else "square"

    image_provider_mod.DEFAULT_ASPECT_RATIO = "square"
    image_provider_mod.ImageGenProvider = ImageGenProvider
    image_provider_mod.error_response = error_response
    image_provider_mod.resolve_aspect_ratio = resolve_aspect_ratio
    image_provider_mod.save_b64_image = lambda _b64, prefix: f"/cache/{prefix}.png"
    image_provider_mod.save_url_image = lambda _url, prefix: f"/cache/{prefix}.png"
    image_provider_mod.success_response = success_response

    tools_mod = types.ModuleType("tools")
    xai_http_mod = types.ModuleType("tools.xai_http")
    xai_http_mod.hermes_xai_user_agent = lambda: "Hermes-Agent/test"
    xai_http_mod.resolve_xai_http_credentials = lambda: dict(xai_creds or {})

    monkeypatch.setitem(sys.modules, "agent", agent_mod)
    monkeypatch.setitem(sys.modules, "agent.image_gen_provider", image_provider_mod)
    monkeypatch.setitem(sys.modules, "tools", tools_mod)
    monkeypatch.setitem(sys.modules, "tools.xai_http", xai_http_mod)


def _load_plugin(monkeypatch, name: str, rel_path: str, *, xai_creds=None):
    _install_provider_stubs(monkeypatch, xai_creds=xai_creds)
    module_name = f"_ws3_image_gen_{name}"
    sys.modules.pop(module_name, None)
    spec = importlib.util.spec_from_file_location(module_name, REPO_ROOT / rel_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    monkeypatch.setitem(sys.modules, module_name, module)
    spec.loader.exec_module(module)
    return module


def test_openai_url_response_is_materialized_before_return(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai",
        "plugins/image_gen/openai/__init__.py",
    )

    captured: dict[str, str] = {}

    def save_url_image(url, *, prefix):
        captured["url"] = url
        captured["prefix"] = prefix
        return "/cache/openai_url.png"

    plugin.save_url_image = save_url_image

    fake_client = SimpleNamespace(
        images=SimpleNamespace(
            generate=lambda **_kwargs: SimpleNamespace(
                data=[SimpleNamespace(b64_json=None, url="https://example.com/signed.png")]
            )
        )
    )
    fake_openai = types.ModuleType("openai")
    fake_openai.OpenAI = lambda: fake_client
    monkeypatch.setitem(sys.modules, "openai", fake_openai)
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")

    result = plugin.OpenAIImageGenProvider().generate("draw a cacheable image")

    assert result["success"] is True
    assert result["image"] == "/cache/openai_url.png"
    assert captured == {
        "url": "https://example.com/signed.png",
        "prefix": "openai_gpt-image-2-medium",
    }


def test_openai_url_cache_failure_falls_back_to_original_url(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai_fallback",
        "plugins/image_gen/openai/__init__.py",
    )

    def fail_cache(_url, *, prefix):
        assert prefix == "openai_gpt-image-2-medium"
        raise OSError("cache offline")

    plugin.save_url_image = fail_cache

    fake_client = SimpleNamespace(
        images=SimpleNamespace(
            generate=lambda **_kwargs: SimpleNamespace(
                data=[SimpleNamespace(b64_json=None, url="https://example.com/signed.png")]
            )
        )
    )
    fake_openai = types.ModuleType("openai")
    fake_openai.OpenAI = lambda: fake_client
    monkeypatch.setitem(sys.modules, "openai", fake_openai)
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")

    result = plugin.OpenAIImageGenProvider().generate("draw a cacheable image")

    assert result["success"] is True
    assert result["image"] == "https://example.com/signed.png"


def test_openai_codex_builds_streaming_responses_payload(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai_codex_payload",
        "plugins/image_gen/openai-codex/__init__.py",
    )

    payload = plugin._build_responses_payload(
        prompt="draw a red barn",
        size="1536x1024",
        quality="high",
    )

    assert payload["model"] == "gpt-5.4"
    assert payload["stream"] is True
    assert payload["input"] == [{
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "draw a red barn"}],
    }]
    assert payload["tools"] == [{
        "type": "image_generation",
        "model": "gpt-image-2",
        "size": "1536x1024",
        "quality": "high",
        "output_format": "png",
        "background": "opaque",
        "partial_images": 1,
    }]
    assert payload["tool_choice"] == {
        "type": "allowed_tools",
        "mode": "required",
        "tools": [{"type": "image_generation"}],
    }


def test_openai_codex_sse_parser_extracts_latest_image(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai_codex_sse",
        "plugins/image_gen/openai-codex/__init__.py",
    )

    class FakeResponse:
        def iter_lines(self):
            return iter([
                b"event: response.image_generation_call.partial_image",
                b'data: {"partial_image_b64": "partial-image"}',
                b"",
                ": keep-alive",
                "event: response.output_item.done",
                'data: {"item": {"type": "image_generation_call", "result": "final-image"}}',
                "",
                "data: [DONE]",
                "",
            ])

    events = list(plugin._iter_sse_json(FakeResponse()))

    assert events[0]["type"] == "response.image_generation_call.partial_image"
    assert plugin._extract_image_b64(events[0]) == "partial-image"
    assert plugin._extract_image_b64(events[1]) == "final-image"
    assert plugin._extract_image_b64({"output": events}) == "final-image"


def test_openai_codex_collects_image_from_raw_httpx_sse(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai_codex_collect",
        "plugins/image_gen/openai-codex/__init__.py",
    )

    aux_mod = types.ModuleType("agent.auxiliary_client")
    aux_mod._codex_cloudflare_headers = lambda token: {"X-Codex-Test": token}
    monkeypatch.setitem(sys.modules, "agent.auxiliary_client", aux_mod)

    captured: dict[str, object] = {}

    class FakeTimeout:
        def __init__(self, *args, **kwargs):
            captured["timeout_args"] = args
            captured["timeout_kwargs"] = kwargs

    class FakeStream:
        def __enter__(self):
            return self

        def __exit__(self, *_exc):
            return False

        def raise_for_status(self):
            return None

        def iter_lines(self):
            return iter([
                b"event: response.image_generation_call.partial_image",
                b'data: {"partial_image_b64": "partial-image"}',
                b"",
                b"event: response.output_item.done",
                b'data: {"item": {"type": "image_generation_call", "result": "final-image"}}',
                b"",
            ])

    class FakeClient:
        def __init__(self, *, timeout, headers):
            captured["timeout"] = timeout
            captured["headers"] = headers

        def __enter__(self):
            return self

        def __exit__(self, *_exc):
            return False

        def stream(self, method, url, *, json):
            captured["method"] = method
            captured["url"] = url
            captured["json"] = json
            return FakeStream()

    fake_httpx = types.ModuleType("httpx")
    fake_httpx.Timeout = FakeTimeout
    fake_httpx.Client = FakeClient
    fake_httpx.HTTPStatusError = RuntimeError
    monkeypatch.setitem(sys.modules, "httpx", fake_httpx)

    image = plugin._collect_image_b64(
        "codex-token",
        prompt="draw with codex auth",
        size="1024x1024",
        quality="medium",
    )

    assert image == "final-image"
    assert captured["method"] == "POST"
    assert captured["url"] == "https://chatgpt.com/backend-api/codex/responses"
    assert captured["headers"] == {
        "X-Codex-Test": "codex-token",
        "Accept": "text/event-stream",
        "Authorization": "Bearer codex-token",
        "Content-Type": "application/json",
    }
    assert captured["json"]["stream"] is True
    assert captured["json"]["tools"][0]["model"] == "gpt-image-2"
    assert captured["timeout_args"] == (300.0,)
    assert captured["timeout_kwargs"]["read"] == 300.0


def test_openai_codex_generate_uses_httpx_dependency_not_openai_sdk(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "openai_codex_generate",
        "plugins/image_gen/openai-codex/__init__.py",
    )

    monkeypatch.setitem(sys.modules, "httpx", types.ModuleType("httpx"))
    monkeypatch.setitem(sys.modules, "openai", None)
    plugin._read_codex_access_token = lambda: "codex-token"

    captured: dict[str, object] = {}

    def collect_image(token, *, prompt, size, quality):
        captured.update({"token": token, "prompt": prompt, "size": size, "quality": quality})
        return "b64-image"

    plugin._collect_image_b64 = collect_image
    plugin.save_b64_image = lambda b64, *, prefix: f"/cache/{prefix}-{b64}.png"

    result = plugin.OpenAICodexImageGenProvider().generate(
        "draw via codex auth",
        aspect_ratio="portrait",
    )

    assert result["success"] is True
    assert result["image"] == "/cache/openai_codex_gpt-image-2-medium-b64-image.png"
    assert captured == {
        "token": "codex-token",
        "prompt": "draw via codex auth",
        "size": "1024x1536",
        "quality": "medium",
    }


def test_xai_uses_shared_credentials_schema_model_and_url_cache(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "xai",
        "plugins/image_gen/xai/__init__.py",
        xai_creds={
            "api_key": "oauth-token",
            "provider": "xai-oauth",
            "base_url": "https://api.x.ai/custom",
        },
    )

    captured: dict[str, object] = {}

    def fake_post(url, *, headers, json, timeout):
        captured.update({"url": url, "headers": headers, "json": json, "timeout": timeout})
        return SimpleNamespace(
            status_code=200,
            raise_for_status=lambda: None,
            json=lambda: {"data": [{"url": "https://imgen.x.ai/xai-tmp-result.png"}]},
        )

    plugin.requests.post = fake_post
    plugin.save_url_image = lambda url, *, prefix: f"/cache/{prefix}.png"
    monkeypatch.setenv("XAI_IMAGE_MODEL", "grok-imagine-image-quality")

    provider = plugin.XAIImageGenProvider()
    schema = provider.get_setup_schema()
    result = provider.generate("draw an OAuth-backed image", aspect_ratio="landscape")

    assert provider.is_available() is True
    assert schema["env_vars"] == []
    assert schema["post_setup"] == "xai_grok"
    assert "OAuth or XAI_API_KEY" in schema["tag"]
    assert result["success"] is True
    assert result["image"] == "/cache/xai_grok-imagine-image-quality.png"
    assert result["model"] == "grok-imagine-image-quality"
    assert captured["url"] == "https://api.x.ai/custom/images/generations"
    assert captured["timeout"] == 120
    assert captured["headers"]["Authorization"] == "Bearer oauth-token"
    assert captured["headers"]["User-Agent"] == "Hermes-Agent/test"
    assert captured["json"] == {
        "model": "grok-imagine-image-quality",
        "prompt": "draw an OAuth-backed image",
        "aspect_ratio": "16:9",
        "resolution": "1k",
    }


def test_xai_missing_credentials_points_to_shared_setup(monkeypatch):
    plugin = _load_plugin(
        monkeypatch,
        "xai_missing",
        "plugins/image_gen/xai/__init__.py",
        xai_creds={},
    )

    result = plugin.XAIImageGenProvider().generate("draw something")

    assert result["success"] is False
    assert result["error_type"] == "missing_api_key"
    assert "Configure xAI OAuth" in result["error"]
