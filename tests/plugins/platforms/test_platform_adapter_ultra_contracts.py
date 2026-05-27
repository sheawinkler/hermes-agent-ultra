from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]


def _source(path: str) -> str:
    return (REPO_ROOT / path).read_text(encoding="utf-8")


def test_google_chat_keeps_rust_first_loop_and_standalone_contracts():
    source = _source("plugins/platforms/google_chat/adapter.py")

    assert not (REPO_ROOT / "agent" / "async_utils.py").exists()
    assert "from agent.async_utils import safe_schedule_threadsafe" not in source
    assert "asyncio.run_coroutine_threadsafe(coro, loop)" in source
    assert "Google Chat standalone send supports text only" in source
    assert "Google Chat standalone send does not support force_document" in source
    assert "trust_env=True" in source


def test_google_chat_oauth_uses_local_private_json_writer():
    source = _source("plugins/platforms/google_chat/oauth.py")

    assert not (REPO_ROOT / "utils.py").exists()
    assert "from utils import atomic_replace" not in source
    assert "def _write_private_json" in source
    assert "stat.S_IRUSR | stat.S_IWUSR" in source
    assert "os.replace(tmp_path, path)" in source
    assert "timeout=15" in source


def test_setup_wizards_use_stdlib_secret_prompt_fallbacks():
    for path in (
        "plugins/platforms/line/adapter.py",
        "plugins/platforms/simplex/adapter.py",
    ):
        source = _source(path)
        assert not (REPO_ROOT / "hermes_cli" / "secret_prompt.py").exists()
        assert "from hermes_cli.secret_prompt import masked_secret_prompt" not in source
        assert "import getpass" in source
        assert "getpass.getpass" in source


def test_line_keeps_local_source_builder_and_proxy_support():
    source = _source("plugins/platforms/line/adapter.py")

    assert "source_obj = self.create_source(" in source
    assert "source_obj = self.build_source(" not in source
    assert source.count("trust_env=True") >= 5


def test_teams_standalone_and_port_contracts():
    source = _source("plugins/platforms/teams/adapter.py")

    assert "def _coerce_port" in source
    assert "self._port = _coerce_port(" in source
    assert "Teams standalone send supports text only" in source
    assert "Teams standalone send does not support force_document" in source
    assert "trust_env=True" in source


def test_policy_only_platform_diffs_are_source_stable():
    discord = _source("plugins/platforms/discord/adapter.py")
    irc = _source("plugins/platforms/irc/adapter.py")
    ntfy = _source("plugins/platforms/ntfy/adapter.py")
    ntfy_yaml = _source("plugins/platforms/ntfy/plugin.yaml")

    assert "await self._handle_message(message)" in discord
    assert 'use_tls_env.lower() in ("1", "true", "yes")' in irc
    assert 'cmd in ("432", "433")' in irc
    assert 'emoji="ntfy"' in ntfy
    assert "Lightweight -" in ntfy_yaml
