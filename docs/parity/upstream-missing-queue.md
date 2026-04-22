# Upstream Missing Patch Queue

Generated: `2026-04-22T08:02:44.348413+00:00`

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
| pending | 3498 |
| ported | 66 |
| superseded | 1023 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `cf9482984e49` | #26 | docs: condense AGENTS.md from 927 to 242 lines |
| `5785bd327266` | #20 | feat: add openai-codex as fallback provider |
| `1404f846a70d` | #20 | feat(cli,gateway): add user-defined quick commands that bypass agent loop |
| `35d57ed752f2` | #20 | refactor: unified OAuth/API-key credential resolution for fallback |
| `a130aa81657d` | #25 | fix: first-time setup skips API key prompts + install.sh sudo on WSL |
| `e6c829384e3b` | #26 | fix: setup wizard shows 60 as default max iterations, should be 90 |
| `3045e29232de` | #26 | fix: default MoA, Home Assistant, and RL Training to off for new installs |
| `167eb824cbde` | #26 | fix: add first_install flag to tools setup for reliable API key prompting |
| `77da3bbc95fe` | #20 | fix: use correct role for summary message in context compressor |
| `7f9dd60c155d` | #26 | fix: first-install tool setup shows all providers + skip options |
| `eb0b01de7b67` | #21 | chore: move agentmail skill to optional-skills, add API key docs |
| `6a49fbb7da5e` | #21 | fix: correct agentmail skill — API key goes in config.yaml env block |
| `4608a7fe4eb0` | #26 | fix: make skills manifest writes atomic |
| `7af33accf100` | #20 | fix: apply secret redaction to file tool outputs |
| `57b48a81ca10` | #26 | feat: add config toggle to disable secret redaction |
| `12f48006314a` | #26 | docs: add security.redact_secrets as commented config section |
| `aaf8f2d2d2db` | #26 | feat: expand secret redaction patterns |
| `aedb773f0d02` | #20 | fix: stabilize system prompt across gateway turns for cache hits |
| `0ce190be0dd7` | #20 | security: enforce 0600/0700 file permissions on sensitive files (inspired by openclaw) |
| `f8240143b60f` | #23 | feat(discord): add DISCORD_ALLOW_BOTS config for bot message filtering (inspired by openclaw) |
| `3b67606c4246` | #26 | fix: custom endpoint provider shows as openrouter in gateway |
| `a6d3becd6a9b` | #21 | feat: update OBLITERATUS skill to v2.0 — match current repo state |
| `d6c710706f1b` | #21 | docs: add real-world testing findings to OBLITERATUS skill |
| `f1a1b58319da` | #26 | fix: hermes setup doesn't update provider when switching to OpenRouter |
| `912efe11b57b` | #20 | fix(tests): add content attribute to fake result objects |
| `1f0944de210b` | #26 | fix: handle non-string content from OpenAI-compatible servers (#759) |
| `732c66b0f325` | #21 | refactor: reorganize skills into sub-categories |
| `654e16187e71` | #21 | feat(mcp): add sampling support — server-initiated LLM requests (#753) |
| `069570d1037f` | #26 | feat: support multiple named custom providers in `hermes model` |
| `d82fcef91b68` | #23 | Improve Discord gateway error handling and logging |
| `f4580b60105f` | #26 | feat: auto-save custom endpoints + removal option |
| `1a2141d04d7f` | #26 | fix: custom providers activate immediately, save model name |
| `c6b75baad073` | #23 | feat: find-nearby skill and Telegram location support |
| `c7541359657e` | #26 | fix: banner wraps in narrow terminals (Kitty, small windows) |
| `46a7d6aeb207` | #23 | Improve Telegram gateway error handling and logging |
| `59705b80cd8e` | #20 | Add tools summary flag to Hermes CLI |
| `1a10eb8cd916` | #26 | fix: off-by-one in setup toggle selection error message |
| `34f8ac2d8570` | #23 | fix: replace blocking time.sleep with await asyncio.sleep in WhatsApp connect |
| `58b756f04c26` | #26 | fix: clean up empty file after failed wl-paste clipboard extraction |
| `c3cf88b202fc` | #20 | feat(cli,gateway): add /personality none and custom personality support |
| `b78b605ba987` | #26 | fix: replace print() with logger.error() in file_tools |
| `34e8d088c21f` | #23 | feat(slack): fix app_mention 404 + add document/video support |
| `5eaf4a3f323c` | #23 | feat: Telegram send_document and send_video for native file attachments |
| `94023e6a85c4` | #20 | feat: conditional skill activation based on tool availability |
| `ac58309dbdb3` | #22 | docs: improve Slack setup guide with channel event subscriptions and scopes |
| `64bec1d06040` | #26 | fix: Slack gateway setup missing event subscriptions and scopes |
| `520aec20e06c` | #26 | fix: add mcp to dev dependencies for test suite |
| `fa2e72ae9c61` | #22 | docs: document docker_volumes config for shared host directories |
| `2d44ed1c5b86` | #20 | test: add comprehensive tests for vision_tools (42 tests) |
| `ef5d811abac6` | #20 | fix: vision auto-detection now falls back to custom/local endpoints |
| `4e3a8a06371f` | #20 | fix: handle empty choices in MCP sampling callback |
| `9abd6bf342aa` | #26 | fix: gateway missing docker_volumes config bridge + list serialization bug |
| `8eabdefa8ac2` | #26 | fix: bring WebResearchEnv up to Atropos environment standards |
| `172a38c344a3` | #24 | fix: Docker persistent bind mounts fail with Permission denied |
| `0d96f1991c5c` | #25 | test: parallelize test suite with pytest-xdist |
| `320f881e0b6d` | #26 | fix: WebResearchEnv compute_reward extracts from AgentResult.messages |
| `bf8350ac1851` | #26 | fix: evaluate() uses full agent loop with tools, not single-turn |
| `b9d55d57196d` | #21 | feat: add pokemon-player skill with battle-tested gameplay tips |
| `975fd86dc429` | #26 | fix: eliminate double LLM judge call and eval buffer pollution |
| `4bc32dc0f140` | #26 | Fix password reader for Windows using msvcrt.getwch() |
| `0a628c1aefd0` | #20 | fix(cli): handle unquoted multi-word session names in -c/--continue and -r/--resume |
| `6ab3ebf1959e` | #21 | Add hermes-atropos-environments skill (bundled) |
| `ee4008431ab0` | #26 | fix: stop terminal border flashing with steady cursor and TUI spinner widget |
| `1aa7badb3c7e` | #20 | fix: add missing Platform.SIGNAL to toolset mappings, update test + config docs |
| `6f3a673aba20` | #26 | fix: restore success-path server_sock.close() before rpc_thread.join() |
| `c0ffd6b70472` | #21 | feat: expand OpenClaw migration to cover all platform channels, provider keys, model/TTS config, shared skills, and daily memory |
| `de6750ed2398` | #20 | feat: add data-driven skin/theme engine for CLI customization |
| `c1775de56f98` | #22 | feat: filesystem checkpoints and /rollback command |
| `b4b46d1b67db` | #26 | docs: comprehensive skin/theme system documentation |
| `f6bc620d3935` | #26 | fix: apply skin colors to local build_welcome_banner in cli.py |
| `1db8609ac99f` | #21 | Fix several documentation typos |
| `4945240fc391` | #26 | feat: add poseidon/sisyphus/charizard skins + banner logo support |
| `c3dec1dcdae5` | #26 | fix(file_tools): pass docker_volumes to sandbox container config |
| `d03de749a1e9` | #26 | fix: add themed hero art for all skins, fix triple-quote syntax |
| `e8cec55fad1f` | #20 | feat(gateway): configurable background process watcher notifications |
| `b0a5fe897456` | #20 | fix: continue after output-length truncation |
| `ca23875575c2` | #26 | fix: unify visibility filter in codex model discovery |
| `580e6ba2ffd9` | #22 | feat: add proper favicon and logo for landing page and docs site |
| `e4adb67ed89e` | #26 | fix(display): rate-limit spinner flushes to prevent line spam under patch_stdout |
| `4bd579f91594` | #20 | fix: normalize max turns config path |
| `694a3ebdd54b` | #20 | fix(code_execution): handle empty enabled_sandbox_tools in schema description |
| `52e3580cd43f` | #20 | refactor: merge new tests into test_code_execution.py |
| `a630ca15de18` | #23 | fix: forward thread_id metadata for Telegram forum topic routing |
| `928bb16da1cb` | #23 | fix: forward thread_id to Telegram adapter + update send_typing signatures |
| `de07aa7c4046` | #20 | feat: add Nous Portal API key provider (#644) |
| `8318a519e6dc` | #26 | fix: pass enabled_tools through handle_function_call to avoid global race |
| `e9742e202f60` | #24 | fix(security): pipe sudo password via stdin instead of shell cmdline |
| `771969f7479c` | #20 | fix: wire up enabled_tools in agent loop + simplify sandbox tool selection |
| `9ea2209a43c1` | #26 | fix: reduce approval/clarify widget flashing + dynamic border widths |
| `e8b19b5826e3` | #26 | fix: cap user-input separator at 120 cols (matches response box) |
| `fadad820dd00` | #20 | fix(config): atomic write for config.yaml to prevent data loss on crash |
| `1caee06b226a` | #26 | fix: tool call repair — auto-lowercase, fuzzy match, helpful error on unknown tool (#520) |
| `cc4ead999adb` | #20 | feat: configurable embedding infrastructure — local (fastembed) + API (OpenAI) (#675) |
| `0fdeffe6c442` | #26 | fix: replace silent exception swallowing with debug logging across tools |
| `e590caf8d870` | #20 | Revert "Merge PR #702: feat: configurable embedding infrastructure — local (fastembed) + API (OpenAI)" |
| `8eefbef91cd7` | #26 | fix: replace ANSI response box with Rich Panel + reduce widget flashing |
| `c358af7861a0` | #21 | Add ASCII video skill to creative category |
| `0229e6b407c8` | #20 | Fix test_analysis_error_logs_exc_info: mock _aux_async_client so download path is reached |
| `74c214e9571a` | #20 | feat(honcho): async memory integration with prefetch pipeline and recallMode |
| `b4af03aea859` | #26 | fix(honcho): clarify API key signup instructions |

