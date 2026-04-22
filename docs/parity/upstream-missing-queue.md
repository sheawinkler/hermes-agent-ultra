# Upstream Missing Patch Queue

Generated: `2026-04-22T07:57:04.000898+00:00`

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
| pending | 3948 |
| ported | 66 |
| superseded | 573 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `b281ecd50ad4` | #26 | Fix: rending issue on /skills command |
| `0cce536fb2c0` | #26 | fix: fileops on mac |
| `588cdacd49e1` | #26 | feat(session): implement session reset policy for messaging platforms |
| `8aa531c7faea` | #26 | fix(gateway): Pass session_db to AIAgent, fixing session_search error |
| `19abbfff9653` | #21 | feat(ocr-and-documents): add OCR and document extraction skills |
| `26a6da27fa72` | #21 | feat(research): add arXiv search skill and documentation |
| `2ff54ae6b35d` | #26 | fix(gateway): Remove session_db from AIAgent instantiation to prevent errors |
| `fec5d59fb3dd` | #26 | feat(gateway): integrate pairing store and event hook system |
| `c77f3da0ceab` | #26 | Cherry-pick 6 bug fixes from PR #76 and update documentation |
| `445d2646a96e` | #21 | Enhance arXiv integration: Add BibTeX generation, ID versioning, and withdrawn paper handling. Update search script to display version information alongside arXiv IDs. |
| `5007a122b273` | #26 | fix(terminal): enhance error logging in cleanup functions with exception info |
| `2ddda5da8940` | #21 | Create DESCRIPTION.md |
| `f9e05218caf6` | #21 | Create SKILL.md |
| `2595d81733eb` | #21 | feat: Add Superpowers software development skills |
| `fbb1923fad18` | #23 | fix(security): patch path traversal, size bypass, and prompt injection in document processing |
| `79bd65034c92` | #20 | fix(agent): handle 413 payload-too-large via compression instead of aborting |
| `f7677ed275e9` | #24 | feat: add docker_volumes config for custom volume mounts |
| `e09ef6b8bc7d` | #26 | feat(gateway): improve model command handling by resolving current model from environment and config file |
| `c92bdd878538` | #26 | fix(cli): improve spinner line clearing to prevent garbled output with prompt_toolkit |
| `8c1f5efcaba6` | #26 | feat(cli): add toolset API key validation and improve checklist display |
| `4f3cb98e5e1c` | #26 | feat(cli): implement platform-specific toolset selection with improved user interface |
| `66d9983d46c0` | #26 | Fix memory tool entry parsing when content contains section sign |
| `07fcb94bc0d9` | #26 | fix(gateway): sync /model and /personality with CLI config.yaml pattern |
| `f14ff3e0417b` | #24 | feat(cli): use user's login shell for command execution to ensure environment consistency |
| `fb7df099e0fd` | #24 | feat(cli): add shell noise filtering and improve command execution with interactive login shell |
| `518826e70c6b` | #25 | fix(docs): standardize terminology and CLI formatting |
| `de0829cec330` | #26 | fix(cli): increase max iterations for child agents and extend API call timeout for improved reliability |
| `0c0a2eb0a279` | #20 | fix(agent): fail fast on Anthropic native base URLs |
| `19f28a633a9e` | #20 | fix(agent): enhance 413 error handling and improve conversation history management in tests |
| `50cb4d5fc7e4` | #20 | fix(agent): update error message for unsupported Anthropic API endpoints to clarify usage of OpenRouter |
| `b7f099beed37` | #25 | feat: add Honcho integration for cross-session user modeling |
| `1d7ce5e063ff` | #26 | feat: integrate honcho-ai package and enhance tool progress callback in delegate_tool |
| `4d8689c10cba` | #26 | feat: add honcho-ai package to dependencies and update extras in uv.lock |
| `0862fa96fdd2` | #21 | refactor(domain-intel): streamline documentation and add CLI tool for domain intelligence operations |
| `de5a88bd976a` | #25 | refactor: migrate tool progress configuration from environment variables to config.yaml |
| `1e463a8e39a8` | #26 | fix: strip <think> blocks from final response to users |
| `35655298e691` | #20 | fix(gateway): prevent TTS voice messages from accumulating across turns |
| `f213620c8bea` | #25 | fix(install): ignore commented lines when checking for existing PATH configuration |
| `c36b256de56a` | #20 | feat: add Home Assistant integration (REST tools + WebSocket gateway) |
| `b32c642af3cf` | #20 | test: add HA integration tests with fake in-process server |
| `2390728cc38b` | #23 | fix: resolve 4 bugs found in HA integration code review |
| `6366177118ec` | #26 | refactor: update context compression configuration to use config.yaml and improve model handling |
| `dfd50ceccd8f` | #20 | fix: preserve Gemini thought_signature in tool call messages |
| `46506769f1e3` | #20 | test: add unit tests for 5 security/logic-critical modules (batch 4) |
| `08250a53a120` | #20 | fix: skills hub dedup prefers higher trust levels + 43 tests |
| `9769e07cd5e5` | #20 | test: add 25 unit tests for trajectory_compressor |
| `1ddf8c26f50d` | #25 | refactor(cli): update max turns configuration precedence and enhance documentation |
| `2205b22409f2` | #26 | fix(headers): update X-OpenRouter-Categories to include 'productivity' |
| `8e0c48e6d25b` | #25 | feat(skills): implement dynamic skill slash commands for CLI and gateway |
| `7b23dbfe6841` | #23 | feat(animation): add support for sending animated GIFs in BasePlatformAdapter and TelegramAdapter |
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

