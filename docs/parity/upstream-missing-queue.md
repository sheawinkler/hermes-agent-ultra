# Upstream Missing Patch Queue

Generated: `2026-04-22T08:13:57.883869+00:00`

- Range: `main..upstream/main`; total commits tracked: `4587`.

| Ticket | Label | Commit Count |
| ---: | --- | ---: |
| #20 | GPAR-01 tests+CI parity | 1631 |
| #21 | GPAR-02 skills parity | 200 |
| #22 | GPAR-03 UX parity | 525 |
| #23 | GPAR-04 gateway/plugin-memory parity | 507 |
| #24 | GPAR-05 environments+parsers+benchmarks | 64 |
| #25 | GPAR-06 packaging/docs/install parity | 152 |
| #26 | GPAR-07 upstream queue backfill | 1508 |

| Disposition | Commit Count |
| --- | ---: |
| pending | 1398 |
| ported | 66 |
| superseded | 3123 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `714809634f1c` | #23 | fix(security): prevent SSRF redirect bypass in Slack adapter |
| `7663c98c1ebd` | #23 | fix: make safe_url_for_log public, add SSRF redirect guards to base.py cache helpers |
| `d7164603dae7` | #20 | feat(auth): add is_provider_explicitly_configured() helper |
| `f3fb3eded483` | #20 | fix(auth): gate Claude Code credential seeding behind explicit provider config |
| `419b719c2b2f` | #20 | fix(auth): make 'auth remove' for claude_code prevent re-seeding |
| `5a1cce53e4b2` | #20 | fix(auxiliary): skip anthropic in fallback chain when not explicitly configured |
| `aedf6c7964fc` | #20 | security(approval): close 4 pattern gaps found by source-grounded audit |
| `26299270323b` | #23 | fix(feishu): wrap image bytes in BytesIO before uploading to lark SDK |
| `e376a9b2c957` | #23 | feat(telegram): support custom base_url for credential proxy |
| `74e883ca3777` | #20 | fix(cli): make /status show gateway-style session status |
| `cc12ab829015` | #23 | fix(matrix): remove eyes reaction on processing complete |
| `58413c411f08` | #20 | test: update Matrix reaction tests for new _send_reaction return type |
| `21bb2547c604` | #23 | fix(matrix): log redact failures and add missing reaction test cases |
| `76a1e6e0fe50` | #23 | feat(discord): add channel_skill_bindings for auto-loading skills per channel |
| `49da1ff1b130` | #20 | test(discord): add tests for channel_skill_bindings resolution |
| `f3ae1d765d75` | #26 | fix: flush stdin after curses/terminal menus to prevent escape sequence leakage (#7167) |
| `6d2fa038377e` | #26 | fix: UTF-8 config encoding, pairing hint, credential_pool key, header normalization (#7174) |
| `0e315a6f02e9` | #23 | fix(telegram): use valid reaction emojis for processing completion (#7175) |
| `5fc5ced9725a` | #20 | fix: add Alibaba/DashScope rate-limit pattern to error classifier |
| `fd3e855d589f` | #20 | fix: pass config_context_length to switch_model context compressor |
| `49bba1096e54` | #20 | fix: opencode-go missing from /model list and improve HERMES_OVERLAYS credential check |
| `0cdf5232aee0` | #26 | fix: always show model selection menu for custom providers |
| `e3b395e17d9f` | #20 | test: add regression tests for custom provider model switching |
| `1662b7f82a2a` | #20 | fix(test): correct mock target for fetch_api_models in custom provider tests |
| `fd5cc6e1b471` | #20 | fix(model): normalize native provider-prefixed model ids |
| `b730c2955af4` | #20 | fix(model): normalize direct provider ids in auxiliary routing |
| `916fbf362cc3` | #20 | fix(model): tighten direct-provider fallback normalization |
| `4a65c9cd08cc` | #20 | fix: profile paths broken in Docker — profiles go to /root/.hermes instead of mounted volume (#7170) |
| `5b63bf7f9a2a` | #23 | feat(gateway): add native Weixin/WeChat support via iLink Bot API |
| `be4f049f46e4` | #22 | fix: salvage follow-ups for Weixin adapter (#6747) |
| `7cec784b64f5` | #22 | fix: complete Weixin platform parity audit — 16 missing integration points |
| `5b8beb0ead2f` | #20 | fix(gateway): handle provider command without config |
| `970192f1838d` | #20 | feat(gateway): add fast mode support to gateway chats |
| `7e60b092746b` | #20 | fix: add _session_model_overrides to test runner fixture |
| `f72faf191c80` | #20 | fix: fall back to default certs when CA bundle path doesn't exist (#7352) |
| `a093eb47f75d` | #20 | fix: propagate child activity to parent during delegate_task (#7295) |
| `7e28b7b5d518` | #26 | fix: parallelize skills browse/search to prevent hanging (#7301) |
| `71036a7a759a` | #20 | fix: handle UnicodeEncodeError with ASCII codec (#6843) |
| `2c99b4e79b4e` | #20 | fix(unicode): sanitize surrogate metadata and allow two-pass retry |
| `c6e1add6f118` | #20 | fix(agent): preserve quoted @file references with spaces |
| `37a1c757164c` | #20 | fix(browser): hardening — dead code, caching, scroll perf, security, thread safety |
| `360b21ce956b` | #23 | fix(gateway): reject file paths in get_command() + file-drop tests (#7356) |
| `0bea60351049` | #26 | fix: handle NoneType request_overrides in fast_mode check (#7350) |
| `f83e86d826e1` | #26 | feat(cli): restore live per-tool elapsed timer in TUI spinner (#7359) |
| `4fb42d01937b` | #20 | fix: per-profile subprocess HOME isolation (#4426) (#7357) |
| `6c115440fde0` | #26 | fix(delegate): sync self.base_url with client_kwargs after credential resolution |
| `7ccdb7436451` | #20 | fix(delegate): make max_concurrent_children configurable + error on excess |
| `363d5d57bee7` | #20 | test: update schema assertion after maxItems removal |
| `f07b35acbae4` | #26 | fix: use raw docstring to suppress invalid escape sequence warning |
| `8bcb8b8e8754` | #20 | feat(providers): add native xAI provider |
| `03f23f10e1ef` | #23 | feat: multi-agent Discord filtering — skip messages addressed to other bots |
| `496e378b1027` | #20 | fix: resolve overlay provider slug mismatch in /model picker (#7373) |
| `ea81aa2eec8c` | #20 | fix: guard api_kwargs in except handler to prevent UnboundLocalError (#7376) |
| `2b0912ab1899` | #25 | fix(install): handle Playwright deps correctly on non-apt systems |
| `8254b820ec8c` | #24 | fix(docker): --init for zombie reaping + sleep infinity for idle-based lifetime |
| `e1167c5c079e` | #26 | fix(deps): add socks extra to httpx for SOCKS proxy support |
| `e8f16f743229` | #26 | fix(docker): add missing skins/plans/workspace dirs to entrypoint |
| `d8cd7974d86c` | #23 | fix(feishu): register group chat member event handlers |
| `3e24ba1656e8` | #22 | feat(matrix): add MATRIX_DM_MENTION_THREADS env var |
| `6f63ba9c8f76` | #20 | fix(mcp): fall back when SIGKILL is unavailable |
| `c1f832a61025` | #26 | fix(tools): guard against ValueError on int() env var and header parsing |
| `475cbce775b8` | #20 | fix(aux): honor api_mode for custom auxiliary endpoints |
| `0e939af7c204` | #20 | fix(patch): harden V4A patch parser and fuzzy match — 9 correctness bugs |
| `a4fc38c5b1ce` | #20 | test: remove dead TestResolveForcedProvider tests (function doesn't exist on main) |
| `c5ab76052892` | #26 | fix(cron): missing field init, unnecessary save, and shutdown cleanup |
| `5b42aecfa765` | #26 | feat(agent): add AIAgent.close() for subprocess cleanup |
| `fbe28352e49e` | #26 | fix(gateway): call agent.close() on session end to prevent zombies |
| `672cc80915ce` | #26 | fix(delegate): close child agent after delegation completes |
| `8414f418565c` | #20 | test: add zombie process cleanup tests |
| `f00dd3169f20` | #26 | fix(gateway): guard _agent_cache_lock access in reset handler |
| `9555a0cf3149` | #26 | fix(gateway): look up expired agents in _agent_cache, add global kill_all |
| `7033dbf5d640` | #20 | test(e2e): add Discord e2e integration tests |
| `79565630b0de` | #20 | refactor(e2e): unify Telegram and Discord e2e tests into parametrized platform fixtures |
| `dab5ec824554` | #20 | test(e2e): add Slack to parametrized e2e platform tests |
| `e8034e2f6adf` | #20 | fix(gateway): replace os.environ session state with contextvars for concurrency safety |
| `baddb6f7174c` | #26 | fix(gateway): derive channel directory platforms from enum instead of hardcoded list (#7450) |
| `9a0c44f908b1` | #20 | fix(nix): gate matrix extra to Linux in [all] profile (#7461) |
| `992422910cc7` | #23 | fix(api): send tool progress as custom SSE event to prevent model corruption (#6972) |
| `842e669a1344` | #20 | fix: activate fallback provider on repeated empty responses + user-visible status (#7505) |
| `fe7e6c156cf3` | #26 | feat: add ContextEngine ABC, refactor ContextCompressor to inherit from it |
| `92382fb00eba` | #20 | feat: wire context engine plugin slot into agent and plugin system |
| `5d8dd622bc71` | #26 | feat: wire context engine tools, session lifecycle, and tool dispatch |
| `3fe693817689` | #26 | fix: robust context engine interface — config selection, plugin discovery, ABC completeness |
| `436dfd5ab5a1` | #20 | fix: no auto-activation + unified hermes plugins UI with provider categories |
| `bff64858f971` | #24 | perf(daytona): bulk upload files in single HTTP call |
| `ac30abd89e45` | #26 | fix(config): bridge container resource settings to env vars |
| `223a0623ee16` | #24 | fix(daytona): use logger.warning instead of warnings.warn for disk cap |
| `97bb64dbbff8` | #20 | test(file_sync): add tests for bulk_upload_fn callback |
| `830040f937e5` | #24 | fix: remove unused BulkUploadFn import from daytona.py |
| `a8fd7257b173` | #20 | feat(gateway): WSL-aware gateway with smart systemd detection (#7510) |
| `1850747172c5` | #20 | refactor(matrix): swap matrix-nio for mautrix-python dependency |
| `8053d48c8df8` | #23 | refactor(matrix): rewrite adapter from matrix-nio to mautrix-python |
| `417e28f9415b` | #20 | test(matrix): update all test mocks for mautrix-python API |
| `d5be23aed7de` | #22 | docs(matrix): update all references from matrix-nio to mautrix |
| `1f3f1200423a` | #23 | fix(matrix): persist E2EE crypto store and fix decrypted event dedup |
| `bc8b93812c0a` | #23 | refactor(matrix): simplify adapter after code review |
| `5d3332dbba55` | #23 | fix(matrix): close leaked sessions on connect failure + HMAC-sign pickle store |
| `be06db71d78f` | #23 | fix(matrix): ignore m.notice messages to prevent bot-to-bot loops |
| `be9198f1e16a` | #20 | fix: guard mautrix imports for gateway-safe fallback + fix test isolation |
| `718e8ad6fa6f` | #20 | feat(delegation): add configurable reasoning_effort for subagents |

