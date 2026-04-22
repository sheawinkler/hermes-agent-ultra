# Upstream Missing Patch Queue

Generated: `2026-04-22T08:13:55.932060+00:00`

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
| pending | 1898 |
| ported | 66 |
| superseded | 2623 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `ed4a605696b5` | #26 | docs: update docstring to mention Fireworks strict validation |
| `65952ac00c66` | #20 | Honor provider reset windows in pooled credential failover |
| `4437354198dc` | #20 | Preserve numeric credential labels in auth removal |
| `441ec4880291` | #26 | style: use module-level re import instead of local import re as _re |
| `0c54da8aafd9` | #23 | feat(gateway): live-stream /update output + interactive prompt buttons (#5180) |
| `cb63b5f381a9` | #21 | feat(skills): add popular-web-designs skill with 54 website design systems (#5194) |
| `4976a8b0668f` | #26 | feat: /model command — models.dev primary database + --provider flag (#5181) |
| `d932980c1a7d` | #21 | Add gitnexus-explorer optional skill (#5208) |
| `51ed7dc2f399` | #20 | feat: save oversized tool results to file instead of destructive truncation (#5210) |
| `e899d6a05d59` | #26 | fix: increase default HERMES_AGENT_TIMEOUT from 10min to 30min |
| `35d280d0bdc1` | #20 | feat: coerce tool call arguments to match JSON Schema types (#5265) |
| `70f798043b65` | #20 | fix: Ollama Cloud auth, /model switch persistence, and alias tab completion |
| `914f7461dc1b` | #20 | fix: add missing shutil import for Matrix E2EE setup |
| `9d7c288d8699` | #23 | fix(matrix): add filesize to nio.upload() for Synapse compatibility |
| `b65e67545a49` | #23 | fix(gateway): stop Matrix/Mattermost reconnect on permanent auth failures |
| `bec02f3731f4` | #23 | fix(matrix): handle encrypted media events and cache decrypted attachments |
| `36e046e843c7` | #26 | fix(gateway): MIME type fallback for Matrix document uploads |
| `c100ad874c34` | #23 | fix(matrix): E2EE cron delivery via live adapter + HTML formatting + origin fallback |
| `20b4060dbfac` | #26 | fix: web_extract fast-fail on scrape timeout + summarizer resilience |
| `534511bebbb1` | #23 | feat(matrix): Tier 1 enhancement — reactions, read receipts, rich formatting, room management |
| `a0a1b86c2edc` | #20 | fix: accept reasoning-only responses without retries — set content to "(empty)" (#5278) |
| `54cb311f4017` | #26 | fix: suppress false 'Unknown toolsets' warning for MCP server names (#5279) |
| `daa4a5acdd20` | #26 | feat: add docs links to setup wizard sections (#5283) |
| `5ff514ec7958` | #26 | fix(security): remove full traceback from cron error output to prevent info leakage |
| `7f853ba7b6ea` | #26 | fix: use logger.exception to preserve traceback in logs and drop unused import |
| `507b63f86b14` | #23 | fix(api-server): pass fallback_model to AIAgent (#4954) |
| `4df2fca2f03e` | #26 | fix(gateway): cap memory flush retries at 3 to prevent infinite loop |
| `6df0f07ff3e9` | #23 | fix: /status command bypasses active-session guard during agent run (#5046) |
| `abf1be564b28` | #26 | fix(deps): include telegram webhook extra in messaging installs (#4915) |
| `aab74b582cbe` | #20 | fix(gateway): replace deprecated launchctl start/stop with kickstart/kill |
| `74ff62f5ac80` | #20 | fix(gateway): use kickstart -k for atomic launchd restart |
| `1d2e34c7ebd4` | #20 | Prevent Telegram polling handoffs and flood-control send failures |
| `afccbf253c36` | #20 | fix: resolve listed messaging targets consistently |
| `4a75aec4335f` | #20 | fix(gateway): resolve Telegram's underscored /commands to skill/plugin keys |
| `e8053e8b937a` | #20 | fix(gateway): surface unknown /commands instead of leaking them to the LLM |
| `6a6ae9a5c36a` | #26 | fix(gateway): correct misleading log text for unknown /commands |
| `0c95e91059c1` | #23 | fix: follow-up fixes for salvaged PRs |
| `6ee90a7cf6ad` | #20 | fix: hermes auth remove now clears env-seeded credentials permanently (#5285) |
| `914a7db44825` | #26 | fix(acp): rename AuthMethod to AuthMethodAgent for agent-client-protocol 0.9.0 |
| `fcdd5447e2ee` | #20 | fix: keep ACP stdout protocol-clean |
| `c71b1d197f44` | #20 | fix(acp): advertise slash commands via ACP protocol |
| `e167ad8f6195` | #26 | feat(delegate): add acp_command/acp_args override to delegate_task |
| `cc2b56b26a9f` | #20 | feat(api): structured run events via /v1/runs SSE endpoint |
| `c6793d6fc3d2` | #23 | fix(gateway): wrap cron helpers with staticmethod to prevent self-binding |
| `ef3bd3b276cd` | #26 | security(approval): fix privilege escalation in gateway once-approval logic |
| `1ebc9324173d` | #26 | fix(security): validate cron deliver platform name to prevent env var enumeration |
| `71a4582bf807` | #26 | fix(security): hoist platform allowlist to module scope as frozenset |
| `567bc7994849` | #26 | fix: clean up cron platform allowlist — add homeassistant, fix import, improve placement |
| `12724e629529` | #22 | feat: progressive subdirectory hint discovery (#5291) |
| `c02c3dc723ae` | #23 | fix(honcho): plugin drift overhaul -- observation config, chunking, setup wizard, docs, dead code cleanup |
| `dd8a42bf7d46` | #23 | feat(plugins): plugin CLI registration system — decouple plugin commands from core |
| `b074b0b13a4f` | #20 | test: add plugin CLI registration tests |
| `0f813c422cdc` | #23 | fix(plugins): only register CLI commands for the active memory provider |
| `583d9f959791` | #23 | fix(honcho): migration guard for observation mode default change |
| `66d0fa177894` | #23 | fix: avoid unnecessary Discord members intent on startup |
| `8d5226753f10` | #20 | fix: add missing ButtonStyle.grey to discord mock for test compatibility |
| `b63fb03f3f63` | #26 | feat(browser): add JS evaluation via browser_console expression parameter (#5303) |
| `4494fba14043` | #20 | feat: OSV malware check for MCP extension packages (#5305) |
| `7409715947a7` | #26 | fix: link subagent sessions to parent and hide from session list |
| `9d885b266c84` | #21 | feat(skills): add manim-video skill for mathematical and technical animations |
| `f116c5907177` | #22 | tui: inherit Python-side rendering via gateway bridge |
| `4c7d5ec778b7` | #26 | tui: add tui arg |
| `256349346600` | #26 | fix: improve timeout debug logging and user-facing diagnostics (#5370) |
| `e9ddfee4fd89` | #20 | fix(plugins): reject plugin names that resolve to the plugins root |
| `fc15f56fc451` | #26 | feat: warn users when loading non-agentic Hermes LLM models (#5378) |
| `fec58ad99e1a` | #26 | fix(gateway): replace wall-clock agent timeout with inactivity-based timeout (#5389) |
| `89c812d1d283` | #23 | feat: shared thread sessions by default — multi-user thread support (#5391) |
| `447ec076a4fa` | #21 | docs(manim-video): expand references with comprehensive Manim CE and 3b1b patterns |
| `b26e7fd43a5f` | #21 | fix(manim-video): recommend monospace fonts — proportional fonts have broken kerning in Pango |
| `0efe7dace751` | #20 | feat: add GPT/Codex execution discipline guidance for tool persistence (#5414) |
| `0365f6202cff` | #20 | feat: show model pricing for OpenRouter and Nous Portal providers |
| `3962bc84b797` | #26 | show cache pricing as well (if supported) |
| `38d844601139` | #20 | feat: implement MCP OAuth 2.1 PKCE client support (#5420) |
| `95a044a2e08d` | #21 | feat(research-paper-writing): fill coverage gaps and integrate patterns from AI-Scientist, GPT-Researcher |
| `aa56df090f7b` | #26 | fix: allow env var overrides for Nous portal/inference URLs (#5419) |
| `ab086a320bd3` | #26 | chore: remove qwen-3.6 free from nous portal model list |
| `786970925e82` | #20 | fix(cli): add missing subprocess.run() timeouts in gateway CLI (#5424) |
| `9ca954a27417` | #20 | fix: mem0 API v2 compat, prefetch context fencing, secret redaction (#5423) |
| `dce5f51c7c43` | #20 | feat: config structure validation — detect malformed YAML at startup (#5426) |
| `9e820dda3791` | #20 | Add request-scoped plugin lifecycle hooks |
| `f530ef1835f4` | #20 | feat(plugins): pre_api_request/post_api_request with narrow payloads |
| `38bcaa1e86df` | #20 | chore: remove langfuse doc, smoketest script, and installed-plugin test |
| `dc9c3cac875d` | #26 | chore: remove redundant local import of normalize_usage |
| `d6ef7fdf9229` | #20 | fix(cron): replace wall-clock timeout with inactivity-based timeout (#5440) |
| `89db3aeb2caa` | #20 | fix(cron): add delivery guidance to cron prompt — stop send_message thrashing (#5444) |
| `9c96f669a151` | #20 | feat: centralized logging, instrumentation, hermes logs CLI, gateway noise fix (#5430) |
| `a2a9ad743148` | #20 | fix: hermes update kills freshly-restarted gateway service |
| `d3d5b895f65e` | #20 | refactor: simplify _get_service_pids — dedupe systemd scopes, fix self-import, harden launchd parsing |
| `6c12999b8c2a` | #26 | fix: bridge tool-calls in copilot-acp adapter |
| `6df4860271e9` | #23 | fix(retaindb): fix API routes, add write queue, dialectic, agent model, file tools |
| `ea8ec27023db` | #23 | fix(retaindb): make project optional, default to 'default' project |
| `574759077067` | #23 | fix: follow-up improvements for salvaged PR #5456 |
| `6f1cb46df982` | #23 | fix: register /queue, /background, /btw as native Discord slash commands (#5477) |
| `79aeaa97e6d3` | #26 | fix: re-order providers,Quick Install, subscription polling |
| `85973e0082fa` | #26 | fix(nous): don't use OAuth access_token as inference API key |
| `6dfab3550100` | #20 | feat(providers): add Google AI Studio (Gemini) as a first-class provider |
| `cc7136b1ac8e` | #20 | fix: update Gemini model catalog + wire models.dev as live model source |
| `a912cd456880` | #21 | docs(manim-video): add 5 new reference files — design thinking, updaters, paper explainer, decorations, production quality |
| `582dbbbbf7c4` | #20 | feat: add grok to TOOL_USE_ENFORCEMENT_MODELS for direct xAI usage (#5595) |
| `f77be22c6506` | #20 | Fix #5211: Preserve dots in OpenCode Go model names |

