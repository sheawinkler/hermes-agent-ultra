# Upstream Missing Patch Queue

Generated: `2026-04-22T07:57:46.842527+00:00`

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
| pending | 3898 |
| ported | 66 |
| superseded | 623 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `10085041cfc1` | #21 | feat: add ascii-art skill for creative text banners and art |
| `ec97f9ad1af2` | #21 | feat(skills): add Solana blockchain skill (converted from tool) |
| `6cbb8f3a0c8a` | #26 | fix: align _apply_delete comment with actual behavior |
| `b7f8a17c24b6` | #20 | fix(gateway): persist transcript changes in /retry, /undo and fix /reset |
| `3f58e47c6391` | #23 | fix: guard POSIX-only process functions for Windows compatibility |
| `c33f8d381b87` | #20 | fix: correct off-by-one in retry exhaustion checks |
| `7f1f4c224817` | #20 | fix(tools): preserve empty content in ReadResult.to_dict() |
| `de101a82028a` | #20 | fix(agent): strip _flush_sentinel from API messages |
| `e87859e82c3c` | #20 | fix(agent): copy conversation_history to avoid mutating caller's list |
| `f7300a858e3d` | #20 | fix(tools): use task-specific glob pattern in disk usage calculation |
| `bf52468a913e` | #26 | fix(gateway): improve MEDIA tag handling to prevent duplication across turns |
| `7f7643cf632c` | #25 | feat(hooks): introduce event hooks system for lifecycle management |
| `500f0eab4a0a` | #20 | refactor(cli): Finalize OpenAI Codex Integration with OAuth |
| `95b0610f36a6` | #26 | refactor(cli, auth): Add Codex/OpenAI OAuth Support - finalized |
| `70dfec9638ad` | #20 | test(redact): add sensitive text redaction |
| `a7c2b9e28093` | #26 | fix(display): enhance memory error detection for tool failures |
| `23d0b7af6a57` | #26 | feat(logging): implement persistent error logging for tool failures |
| `1db559829485` | #20 | feat(tests): add live integration tests for file operations and shell noise filtering |
| `dd69f16c3e06` | #20 | feat(gateway): expose subagent tool calls and thinking to user (fixes #169) (#186) |
| `4ec386cc724f` | #20 | fix(display): use spaces instead of ANSI \033[K in print_above() for prompt_toolkit compat |
| `41d8a802268d` | #20 | fix(display): fix subagent progress tree-view visual nits |
| `ed0e860abb09` | #20 | fix(honcho): auto-enable when API key is present |
| `30efc263ffca` | #26 | feat(cli): add /compress command for manual conversation context compression |
| `177be32b7f91` | #26 | feat(cli): add /usage command to display session token usage |
| `93f5fd80b8b0` | #26 | feat(gateway): add /compress and /usage commands for conversation management |
| `54147474d3f3` | #23 | feat(gateway): include Discord channel topic in session context |
| `3b745633e4f5` | #20 | test: add unit tests for 8 untested modules (batch 3) (#191) |
| `11f5c1ecf016` | #20 | fix(tests): use bare @pytest.mark.asyncio for hook emit tests |
| `440d33eec403` | #26 | Improve error handling and type hints in session_search_tool |
| `196a13f3dcb4` | #26 | Improve error handling and validation in transcription_tools |
| `834e25a662ab` | #26 | feat(batch_runner): enhance prompt processing with optional container image support |
| `dda9f3e734c2` | #26 | fix(process_registry): ensure unbuffered output for subprocesses |
| `c84d5ce738be` | #26 | refactor(terminal_tool): clarify foreground and background process usage |
| `92da8e7e6244` | #26 | feat(agent): enhance reasoning handling and configuration |
| `72963e9ccbd1` | #25 | fix(install): prevent interactive prompts during non-interactive installs |
| `75a92a3f82b1` | #26 | refactor(cli): improve header formatting and description truncation |
| `8bc2de4ab696` | #20 | feat(provider-routing): add OpenRouter provider routing configuration |
| `c2d8d1728545` | #21 | feat(skills): add DuckDuckGo search skill as Firecrawl fallback |
| `5e598a588f6c` | #20 | refactor(auth): transition Codex OAuth tokens to Hermes auth store |
| `e5893075f9b5` | #20 | feat(agent): add summary handling for reasoning items |
| `7b38afc179d6` | #26 | fix(auth): handle session expiration and re-authentication in Nous Portal |
| `5e5e0efc6088` | #20 | Fix nous refresh token rotation failure in case where api key mint/retrieval fails |
| `47289ba6f133` | #26 | feat(agent): include system prompt in agent status output |
| `0512ada793b3` | #26 | feat(agent): include tools in agent status output |
| `698b35933e4f` | #26 | fix: /retry, /undo, /compress, and /reset gateway commands (#210) |
| `45d132d098a5` | #26 | fix(agent): remove preview truncation in assistant message output |
| `e2b8740fcf54` | #26 | fix: load_cli_config() now carries over non-default config keys |
| `7a0b37712ff2` | #26 | fix(agent): strip finish_reason from assistant messages to fix Mistral 422 errors (#253) |
| `1ad930cbd061` | #26 | fix(delegate_tool): increase DEFAULT_MAX_ITERATIONS from 25 to 50 to enhance processing capabilities |
| `14396e3fe777` | #26 | fix(delegate_tool): update max_iterations default from 25 to 50 for improved task handling |
| `6bf3aad62ec6` | #25 | fix(delegate_tool): update max_iterations in documentation and example config to reflect default value of 50 |
| `b1bf11b0fed1` | #26 | fix(setup): handle TerminalMenu init failures with safe fallback |
| `e265006fd6c9` | #20 | test: add coverage for chat_topic in SessionSource and session context prompt |
| `d2ec5aaacf7c` | #26 | fix(registry): preserve full traceback on tool dispatch errors |
| `866fd9476bf3` | #25 | fix(docker): remove --read-only and allow exec on /tmp for package installs |
| `c574a4d0862c` | #26 | fix(batch_runner): log traceback when worker raises during imap_unordered |
| `afb680b50dc2` | #20 | fix(cli): fix max_turns comment and test for correct priority order |
| `25c65bc99eea` | #20 | fix(agent): handle None content in context compressor (fixes #211) |
| `33ab5cec825f` | #26 | fix: handle None message content across codebase (fixes #276) |
| `234b67f5fd7d` | #20 | fix: mock time in retry exhaustion tests to prevent backoff sleep |
| `fd335a4e26eb` | #26 | fix: add missing dangerous command patterns in approval.py |
| `ca5525bcd7df` | #20 | fix(tests): isolate HERMES_HOME in tests and adjust log directory for debug session |
| `8c48bb080fb6` | #26 | refactor: remove unnecessary single-element loop in disk usage calc |
| `7862e7010cbd` | #20 | test: add additional multiline bypass tests for find patterns |
| `3c13feed4c39` | #26 | feat: show detailed tool call args in gateway based on config |
| `b603b6e1c973` | #26 | fix(cli): throttle UI invalidate to prevent terminal blinking on SSH |
| `6789084ec0bc` | #20 | Fix ClawHub Skills Hub adapter for updated API |
| `3c252ae44b52` | #20 | feat: add MCP (Model Context Protocol) client support |
| `0eb0bec74cac` | #26 | feat(gateway): add MCP server shutdown on gateway exit |
| `aa2ecaef29fd` | #20 | fix: resolve orphan subprocess leak on MCP server shutdown |
| `593c549bc466` | #26 | fix: make discover_mcp_tools idempotent to prevent duplicate connections |
| `151e8d896ca2` | #20 | fix(tests): isolate discover_mcp_tools tests from global _servers state |
| `11a2ecb936d6` | #20 | fix: resolve thread safety issues and shutdown deadlock in MCP client |
| `358839626370` | #23 | feat(whatsapp): native media sending — images, videos, documents |
| `11615014a4ec` | #20 | fix: eliminate shell noise from terminal output with fence markers |
| `60532361583b` | #20 | fix: prioritize OPENROUTER_API_KEY over OPENAI_API_KEY |
| `ee541c84f19b` | #26 | fix(cron): close lock_fd on failed flock to prevent fd leak |
| `ac6d747fa610` | #26 | Make batch_runner checkpoint incremental and atomic |
| `5fa3e24b7620` | #26 | Make process_registry checkpoint writes atomic |
| `c6b3b8c84722` | #26 | docs: add VISION.md brainstorming/roadmap doc |
| `14b0ad95c6ae` | #25 | docs: enhance WhatsApp setup instructions and introduce mode selection |
| `64ff8f065b1f` | #20 | feat(mcp): add HTTP transport, reconnection, security hardening |
| `63f5e14c6993` | #21 | docs: add comprehensive MCP documentation and examples |
| `60effcfc4427` | #20 | fix(mcp): parallel discovery, user-visible logging, config validation |
| `7df14227a957` | #20 | feat(mcp): banner integration, /reload-mcp command, resources & prompts |
| `eec31b008910` | #26 | fix(mcp): /reload-mcp now updates agent tools + injects history message |
| `3ead3401e0b0` | #26 | fix(mcp): persist updated tools to session log immediately after reload |
| `de59d91add14` | #25 | feat: Windows native support via Git Bash |
| `daedec6957df` | #23 | fix: Telegram adapter crash on Windows when library not installed (#304) |
| `84e45b5c402c` | #26 | feat: tabbed platform installer on landing page |
| `bdf475851025` | #26 | fix: show uv error on Python install failure, add fallback detection |
| `cdf5375b9a00` | #26 | fix: PowerShell NativeCommandError on git stderr output |
| `f08ad94d4d8a` | #20 | fix: correct typo 'Grup' -> 'Group' in test section headers |
| `245c76651285` | #26 | fix: remove 2>&1 from git commands in PowerShell installer |
| `5f29e7b63c7d` | #21 | fix: rename misspelled directory 'fouth-edition' to 'fourth-edition' |
| `a718aed1be1b` | #21 | fix: rename misspelled directory 'fouth-edition' to 'fourth-edition' |
| `4cc431afabe8` | #26 | fix: setup wizard skipping provider selection on fresh install |
| `8b520f98485b` | #21 | fix: rename misspelled directory 'fouth-edition' to 'fourth-edition' |
| `d10108f8caf5` | #21 | fix: rename misspelled directory 'fouth-edition' to 'fourth-edition' |
| `5749f5809c49` | #26 | fix: explicit UTF-8 encoding for .env file operations (Windows only) |

