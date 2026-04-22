# Upstream Missing Patch Queue

Generated: `2026-04-22T08:03:14.308769+00:00`

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
| pending | 3398 |
| ported | 66 |
| superseded | 1123 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `6782249df935` | #26 | fix(honcho): rewrite tokens and peer CLI help for clarity |
| `c1228e9a4a73` | #20 | refactor(honcho): rename recallMode "auto" to "hybrid" |
| `792be0e8e3fc` | #26 | feat(honcho): add honcho_conclude tool for writing facts back to memory |
| `0cb639d47235` | #26 | refactor(honcho): rename query_user_context to honcho_context |
| `c047c03e82aa` | #26 | feat(honcho): honcho_context can query any peer (user or ai) |
| `87cc5287a878` | #20 | fix(honcho): enforce local mode and cache-safe warmup |
| `87349b9bc1af` | #20 | fix(gateway): persist Honcho managers across session requests |
| `c90ba029ce79` | #22 | refactor(honcho): write all host-scoped settings into hosts block |
| `4c54c2709c1c` | #22 | Revert "refactor(honcho): write all host-scoped settings into hosts block" |
| `d94519c5ba24` | #20 | fix(skills): classify local skills separately in skills list |
| `d04b9f4dc56a` | #23 | fix(signal): use media_urls/media_types instead of non-existent image_paths/audio_path/document_paths |
| `cea78c5e278c` | #23 | fix(gateway): add metadata param to _keep_typing and base send_typing |
| `d6ab35c1a3a4` | #23 | fix(signal): align send() signature with base class (content, reply_to, metadata) |
| `c1171fe66645` | #20 | fix: eliminate 3x SQLite message duplication in gateway sessions (#860) |
| `ad7a16dca64a` | #26 | fix: remove left/right borders from response box for easier copy-paste |
| `a458b535c97f` | #20 | fix: improve read-loop detection — consecutive-only, correct thresholds, fix bugs |
| `03a4f184e6c7` | #20 | fix: call _stop_training_run on early-return failure paths |
| `2dddfce08c20` | #20 | fix: log prefill parse errors + clean up cron scheduler tests |
| `145c57fc01e1` | #20 | fix: provider selection not persisting when switching via hermes model |
| `d502952bace2` | #20 | fix(cli): add loading indicators for slow slash commands |
| `d41a214c1a86` | #21 | feat(skills): add official optional 1password skill |
| `24479625a2e9` | #20 | fix: Docker backend fails when docker is not in PATH (macOS gateway) |
| `23270d41b947` | #26 | feat: add --quiet/-Q flag for programmatic single-query mode |
| `2d80ef78722f` | #26 | fix: _init_agent returns bool, not agent — fix quiet mode crash |
| `67b94702075a` | #26 | fix: reduce premature gateway compression on tool-heavy sessions |
| `1518734e591e` | #26 | fix: sort Nous Portal model list (opus first, sonnet lower) |
| `58dbd81f0352` | #26 | fix: use actual API token counts for gateway compression pre-check |
| `5eb62ef4238f` | #20 | test(gateway): add regression test for /retry response fix |
| `909e048ad42c` | #20 | fix: integration hardening for gateway token tracking |
| `8eb9eed074a0` | #26 | feat(ux): improve /help formatting with command categories (#640) |
| `2b244762e14a` | #26 | feat: add missing commands to categorized /help |
| `a9241f3e3e22` | #20 | fix: head+tail truncation for execute_code stdout |
| `21ff0d39ad00` | #20 | feat: iteration budget pressure via tool result injection |
| `aead9c8eadaa` | #23 | chore: remove unnecessary pragma comments from Telegram adapter |
| `331af8df23e4` | #26 | fix: clean up tools --summary output and type annotations |
| `ae1c11c5a512` | #20 | fix(cli): resolve duplicate 'skills' subparser crash on Python 3.11+ |
| `fbfdde496bbe` | #26 | docs: update AGENTS.md with new files and test count |
| `0d6b25274c6d` | #20 | fix(gateway): isolate telegram forum topic sessions |
| `de2b881886bb` | #20 | test(cron): cover topic thread delivery metadata |
| `f5324f9aa500` | #26 | fix: initialize self.config in HermesCLI to fix AttributeError on slash commands |
| `bd2606a5760a` | #26 | fix: initialize self.config in HermesCLI to fix AttributeError on slash commands |
| `b8067ac27e7a` | #20 | feat: add /background command to gateway and CLI commands registry |
| `4523cc09cfe6` | #26 | fix(terminal): validate env var types with clear error messages |
| `f1510ec33e9b` | #20 | test(terminal): add tests for env var validation in _get_env_config |
| `4864a5684a1c` | #20 | refactor: extract shared curses checklist, fix skill discovery perf |
| `69090d6da1cf` | #23 | fix: add **kwargs to base/telegram media send methods for metadata routing |
| `a82ce6029466` | #20 | fix: add missing Responses API parameters for Codex provider |
| `1d4a23fa6c83` | #26 | fix: add missing packages to setuptools config for non-editable installs |
| `9149c34a26d2` | #23 | refactor(slack): replace print statements with structured logging |
| `4d873f77c1a7` | #22 | feat(cli): add /reasoning command for effort level and display toggle |
| `9423fda5cb57` | #20 | feat: configurable subagent provider:model with full credential resolution |
| `bdcf247efedf` | #23 | feat: add email gateway platform (IMAP/SMTP) |
| `184aa5b2b386` | #20 | fix: tighten exc_info assertion in vision test (from PR #803) |
| `eac5f8f40f9d` | #22 | fix: wire email platform into toolset mappings + add documentation |
| `2c97bf393656` | #20 | Add tests for atropos tool calling integration |
| `d7f4db53f585` | #24 | fix: Modal sandbox eval infra (9 fixes for TBLite baseline) |
| `b03aefaf20fc` | #20 | test: 13 tests for Modal sandbox infra fixes |
| `ed27b826c576` | #24 | feat: add eval_concurrency limit + Docker local config for TBLite |
| `ee4b20b55ba2` | #20 | test: 9 agent loop tool-calling integration tests |
| `84147f4d815b` | #26 | refactor: update to new atropos tool-calling API |
| `1f9e7cd65989` | #20 | test: 5 vLLM integration tests + fallback tool call parser |
| `93333387d60f` | #26 | fix: handle dict and object tool_calls in agent loop |
| `13f545967010` | #24 | fix: use ManagedServer for vLLM in TBLite eval + local_vllm config |
| `366de72a3800` | #24 | add a local vllm instance |
| `0f53275169f1` | #20 | test: skip atropos-dependent tests when atroposlib not installed |
| `d198a647e2f9` | #20 | fix: guard all atroposlib imports for CI without atropos installed |
| `59b53f0a2313` | #20 | fix: skip tests when atroposlib/minisweagent unavailable in CI |
| `d2dee43825e3` | #26 | fix: allow tool_choice, parallel_tool_calls, prompt_cache_key in codex preflight |
| `683c8b24d41f` | #26 | fix: reduce max_retries to 3 and make ValueError/TypeError non-retryable |
| `c64efa92607b` | #26 | fix: smart vision setup that respects the user's chosen provider |
| `efb780c75495` | #26 | Revert "fix: smart vision setup that respects the user's chosen provider" |
| `a54405e339d8` | #20 | fix: proactive compression after large tool results + Anthropic error detection |
| `4a8f23eddff6` | #20 | fix: correctly track failed MCP server connections in discovery |
| `b4a100dfc07d` | #26 | fix(doctor): skip /models health check for MiniMax providers |
| `605ba4adea51` | #26 | fix(cron): interpret naive timestamps as local time in due-job checks |
| `a5ffa1278c98` | #20 | test(cron): add regression tests for _ensure_aware timezone conversion |
| `047b118299fb` | #20 | fix(honcho): resolve review blockers for merge |
| `82113f1f1edd` | #21 | docs: conditional skill activation — tag duckduckgo-search as web fallback and add documentation |
| `a182d1277873` | #21 | Fix several documentation typos across training references |
| `66c0b719de61` | #26 | fix(gateway): pass model to temporary AIAgent instances |
| `3667138d05da` | #26 | fix(config): atomic write for .env to prevent API key loss on crash |
| `b66c8b409c71` | #26 | fix(vision): log error when vision client is unavailable |
| `01bec407245f` | #26 | refactor(gateway): consolidate model resolution via _resolve_gateway_model() |
| `91101065bb37` | #26 | fix: improve git error logging in checkpoint manager |
| `11825ccefaba` | #23 | feat(gateway): thread-aware free-response routing for Discord |
| `41fa4fbaa5dc` | #26 | fix: add exc_info=True to image generation error logging |
| `452593319b39` | #20 | fix(setup): preserve provider metadata during model selection |
| `a8409a161f1a` | #20 | fix: guard all print() calls against OSError with _SafeWriter |
| `d987ff54a1c9` | #20 | fix: change session_strategy default from per-directory to per-session |
| `44bf859c3b45` | #20 | feat: offer OpenClaw migration during first-time setup wizard |
| `4f427167ac49` | #20 | chore: clean OpenClaw migration follow-up |
| `071263944100` | #20 | test: verify reloaded config drives setup after migration |
| `3c813535a746` | #26 | fix(honcho): scope config writes to hosts.hermes, not root |
| `2d35016b94a9` | #20 | fix(honcho): harden tool gating and migration peer routing |
| `8805e705a7e1` | #26 | feat: centralized provider router + fix Codex vision bypass + vision error handling |
| `07f09ecd83fb` | #26 | refactor: route ad-hoc LLM consumers through centralized provider router |
| `013cc4d2fcc4` | #20 | chore: remove nous-api provider (API key path) |
| `0aa31cd3cb81` | #20 | feat: call_llm/async_call_llm + config slots + migrate all consumers |
| `29ef69c70332` | #20 | fix: update all test mocks for call_llm migration |
| `a29801286ff0` | #20 | refactor: route main agent client + fallback through centralized router |

