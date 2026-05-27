import ast
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def read_source(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def assert_parses(path: str) -> str:
    source = read_source(path)
    ast.parse(source, filename=path)
    return source


def test_hindsight_keeps_rust_first_scheduler_and_secret_prompt_fallbacks():
    source = assert_parses("plugins/memory/hindsight/__init__.py")

    assert not (REPO_ROOT / "agent" / "async_utils.py").exists()
    assert not (REPO_ROOT / "hermes_cli" / "secret_prompt.py").exists()
    assert "from agent.async_utils import safe_schedule_threadsafe" not in source
    assert "from hermes_cli.secret_prompt import masked_secret_prompt" not in source
    assert "asyncio.run_coroutine_threadsafe(coro, loop)" in source
    assert "import getpass" in source
    assert "getpass.getpass(prompt=\"\")" in source


def test_honcho_cli_uses_stdlib_secret_prompt_fallback():
    source = assert_parses("plugins/memory/honcho/cli.py")

    assert not (REPO_ROOT / "hermes_cli" / "secret_prompt.py").exists()
    assert "from hermes_cli.secret_prompt import masked_secret_prompt" not in source
    assert "import getpass" in source
    assert "getpass.getpass(prompt=\"\")" in source
    assert "parsed.scheme in (\"http\", \"https\")" in source
    assert "answer.lower() not in (\"y\", \"yes\")" in source


def test_honcho_client_avoids_private_profile_helper_import():
    source = assert_parses("plugins/memory/honcho/client.py")

    assert not (REPO_ROOT / "hermes_cli" / "profiles.py").exists()
    assert "from hermes_cli.profiles import _get_default_hermes_home" not in source
    assert 'Path.home() / ".hermes" / "honcho.json"' in source
    assert 'profile not in ("default", "custom")' in source


def test_tuple_only_memory_provider_diffs_are_runtime_equivalent_guards():
    byterover = assert_parses("plugins/memory/byterover/__init__.py")
    honcho = assert_parses("plugins/memory/honcho/__init__.py")
    supermemory = assert_parses("plugins/memory/supermemory/__init__.py")

    assert 'action not in ("add", "replace")' in byterover
    assert 'role in ("user", "assistant")' in byterover
    assert 'agent_context in ("cron", "flush")' in honcho
    assert 'self._recall_mode in ("context", "hybrid")' in honcho
    assert 'lowered in ("true", "1", "yes", "y", "on")' in supermemory
    assert 'agent_context not in ("cron", "flush", "subagent")' in supermemory
    assert 'role not in ("user", "assistant")' in supermemory
