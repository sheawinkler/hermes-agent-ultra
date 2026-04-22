# Upstream Missing Patch Queue

Generated: `2026-04-22T08:01:43.835943+00:00`

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
| pending | 3698 |
| ported | 66 |
| superseded | 823 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `c886333d3218` | #20 | feat: smart context length probing with persistent caching + banner display |
| `884c8ea70a35` | #26 | chore: add openai/gpt-5.4 to OpenRouter preferred models list |
| `363633e2bafc` | #22 | fix: allow self-hosted Firecrawl without API key + add self-hosting docs |
| `9f4542b3dbd2` | #26 | fix: require Python 3.11+ in pyproject.toml |
| `399562a7d1cf` | #20 | feat: clipboard image paste in CLI (Cmd+V / Ctrl+V) |
| `ffc752a79ed3` | #20 | test: improve clipboard tests with realistic scenarios and multimodal coverage |
| `e2a834578dda` | #20 | refactor: extract clipboard methods + comprehensive tests (37 tests) |
| `e9f05b352497` | #20 | test: comprehensive tests for model metadata + firecrawl config |
| `a44e041acf39` | #20 | test: strengthen assertions across 7 test files (batch 1) |
| `5c867fd79fc5` | #20 | test: strengthen assertions across 3 more test files (batch 2) |
| `8253b54be93d` | #20 | test: strengthen assertions in skill_manager + memory_tool (batch 3) |
| `2317d115cd01` | #20 | fix: clipboard image paste on WSL2, Wayland, and VSCode terminal |
| `014a5b712d4c` | #26 | fix: prevent duplicate gateway instances from running simultaneously |
| `e93b4d1dcdcc` | #26 | feat: Alt+V keybinding for clipboard image paste |
| `6055adbe1b5f` | #26 | fix(config): route API keys and tokens to .env instead of config.yaml |
| `32636ecf8a75` | #22 | Update MiniMax model ID from m2.1 to m2.5 |
| `2387465dcc2e` | #26 | chore: add openai/gpt-5.4-pro and stepfun/step-3.5-flash to OpenRouter models |
| `8c80b9631805` | #26 | chore: update OpenRouter model list |
| `f2e24faaca15` | #21 | feat: optional skills — official skills shipped but not activated by default |
| `ec0fe3242aac` | #26 | feat: 'hermes skills browse' — paginated browsing of all hub skills |
| `f6f3d1de9b81` | #21 | fix: review fixes — path traversal guard, trust_style consistency, edge cases |
| `5ce2c47d603a` | #22 | docs: update all docs for optional-skills and browse command |
| `efec4fcaabf9` | #20 | feat(execute_code): add json_parse, shell_quote, retry helpers to sandbox |
| `8481fdcf08b0` | #22 | docs: complete Daytona backend documentation coverage |
| `3982fcf09517` | #20 | fix: sync execute_code sandbox stubs with real tool schemas |
| `3670089a42a5` | #26 | docs: add Daytona to batch_runner, process_registry, agent_loop, tool_context |
| `b89eb2917401` | #20 | fix: correct mock tool name 'search' → 'search_files' in test_code_execution |
| `32dbd31b9a87` | #26 | fix: restrict .env file permissions to owner-only |
| `c30967806c75` | #20 | test: add 26 tests for set_config_value secret routing |
| `2dbbedc05a7f` | #22 | docs: rebrand messaging — 'the self-improving AI agent' |
| `dc55f493bec8` | #24 | fix: add missing re.DOTALL to DeepSeek V3.1 parser (same bug as V3) |
| `453e0677d63a` | #26 | fix: use regex for search output parsing to handle Windows drive-letter paths |
| `b4873a5de700` | #26 | fix(setup): Escape skips instead of exiting, add control hints to all prompts |
| `d63b363cde77` | #20 | refactor: extract atomic_json_write helper, add 24 checkpoint tests |
| `7a0544ab57a1` | #24 | fix: three small inconsistencies across cron, gateway, and daytona |
| `566aeaeefac4` | #26 | Make skill file writes atomic |
| `b52b37ae6481` | #20 | feat: add /insights command with usage analytics and cost estimation |
| `75f523f5c033` | #20 | fix: unknown/custom models get zero cost instead of fake estimates |
| `585f8528b217` | #20 | fix: deep review — prefix matching, tool_calls extraction, query perf, serialization |
| `1755a9e38a77` | #21 | Design agent migration skill for Hermes Agent from OpenClaw \| Run successful dry tests with reports |
| `ab0f4126cf97` | #21 | fix: restore all removed bundled skills + fix skills sync system |
| `4f56e31dc741` | #20 | fix: track origin hashes in skills manifest to preserve user modifications |
| `f2fdde5ba4f5` | #26 | fix: show user-modified skills count in hermes update output |
| `8ae4a6f824e7` | #26 | fix: improve handling of empty responses after tool calls |
| `211b55815eb6` | #20 | fix: prevent data loss in skills sync on copy/update failure |
| `3b43f7267a1f` | #20 | fix: count actual tool calls instead of tool-related messages |
| `2a680996752c` | #20 | fix(tests): isolate tests from user ~/.hermes/ config and SOUL.md |
| `94053d75a64a` | #20 | fix: custom endpoint no longer leaks OPENROUTER_API_KEY (#560) |
| `33cfe1515dc2` | #20 | fix: sanitize FTS5 queries and close mirror DB connections |
| `f75b1d21b4c8` | #26 | fix: execute_code and delegate_task now respect disabled toolsets |
| `a857321463ce` | #26 | fix(code-execution): close server socket in finally block to prevent fd leak |
| `bc091eb7ef1f` | #20 | fix: implement Nous credential refresh on 401 error for retry logic |
| `388dd4789c45` | #22 | feat: add z.ai/GLM, Kimi/Moonshot, MiniMax as first-class providers |
| `53b4b7651a55` | #21 | Add official OpenClaw migration skill for Hermes Agent |
| `9742f11fda2a` | #26 | chore: add context lengths for Kimi and MiniMax models |
| `e2821effb5cc` | #26 | feat: add direct API-key providers as auxiliary client fallbacks |
| `82d7e9429e9d` | #26 | chore: add GLM/Kimi/MiniMax models to insights pricing (zero cost) |
| `b4fbb6fe1009` | #24 | feat: add YC-Bench long-horizon agent benchmark environment |
| `560911788260` | #20 | fix(doctor): recognize OPENAI_API_KEY custom endpoint config |
| `ce28f847ce16` | #24 | fix: update OpenRouter model names for yc-bench config |
| `8bf28e144146` | #26 | fix(setup): prevent OpenRouter model list fallback for Nous provider |
| `ab9cadfeee85` | #26 | feat: modular setup wizard with section subcommands and tool-first UX |
| `0111c9848d45` | #26 | fix: remove ANSI codes and em dashes from menu labels |
| `82b18e8ac22b` | #26 | feat: unify hermes tools and hermes setup tools into single flow |
| `a62a137a4fb6` | #26 | fix: handle dict-format model config in setup wizard display |
| `9dac85b069cb` | #26 | fix: uv pip install fails outside venv in setup wizard |
| `f55f625277db` | #26 | chore: reorder terminal backends in setup wizard |
| `55a21fe37b36` | #22 | docs: add Environments, Benchmarks & Data Generation guide |
| `348936752a37` | #26 | fix: simplify timezone migration to use os.getenv directly |
| `f668e9fc753e` | #21 | feat: platform-conditional skill loading + Apple/macOS skills |
| `d29249b8fa07` | #26 | feat: local browser backend — zero-cost headless Chromium via agent-browser |
| `55c70f3508c6` | #23 | fix: strip MarkdownV2 escapes from Telegram plaintext fallback |
| `caab1cf4536f` | #26 | fix: update setup/config UI for local browser mode |
| `86caa8539c79` | #26 | Improve TTS error handling and logging |
| `064c009deb92` | #26 | feat: show update-available notice in CLI banner |
| `ce7e7fef30f8` | #21 | docs(skill): expand duckduckgo-search with DDGS Python API coverage |
| `5cdcb9e26f83` | #23 | fix: strip MarkdownV2 italic markers in Telegram plaintext fallback |
| `40bc7216e1c6` | #26 | fix(security): use in-memory set for permanent allowlist save |
| `5da55ea1e322` | #26 | fix: sanitize orphaned tool-call/result pairs in message compression |
| `0a8239671816` | #26 | feat: shared iteration budget across parent + subagents |
| `451a007fb11b` | #20 | fix(tests): isolate max_turns tests from CI env and update default to 90 |
| `ee7d8c56c71c` | #20 | fix: prevent data loss in clipboard PNG conversion when ImageMagick fails |
| `70cffa4d3b49` | #26 | fix: return "deny" on approval callback timeout instead of None |
| `4d34427cc79d` | #26 | fix: update model version in agent configurations |
| `ae4644f49513` | #26 | Fix Ruff lint warnings (unused imports and unnecessary f-strings) |
| `8c26a057a3a6` | #26 | fix: reset all retry counters at start of run_conversation() |
| `5a711f32b13e` | #26 | fix: enhance payload and context compression handling |
| `fb0f579b165d` | #26 | refactor: remove model parameter from delegate_task function |
| `b0b19fdeb1f0` | #26 | fix(session): atomic write for sessions.json to prevent data loss on crash |
| `48e0dc87916e` | #26 | feat: implement Z.AI endpoint detection for API key validation |
| `23e84de8308d` | #26 | refactor: remove model parameter from AIAgent initialization |
| `ee5daba061e5` | #26 | fix: resolve systemd restart loop with --replace flag (#576) |
| `b84f9e410c01` | #20 | feat: default reasoning effort from xhigh to medium |
| `e64d646bad67` | #26 | Critical: fix bug in new subagent tool call budget to not be session-level but tool call loop level |
| `d80c30cc92fa` | #20 | feat(gateway): proactive async memory flush on session expiry |
| `8c0f8baf326c` | #26 | feat(delegate_tool): add additional parameters for child agent configuration |
| `24f6a193e727` | #20 | fix: remove stale 'model' assertion from delegate_task schema test |
| `5baae0df8897` | #26 | feat(scheduler): enhance job configuration with reasoning effort, prefill messages, and provider routing |
| `306d92a9d7c5` | #26 | refactor(context_compressor): improve summary generation logic and error handling |
| `19459b762314` | #26 | Improve skills tool error handling |

