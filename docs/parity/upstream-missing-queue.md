# Upstream Missing Patch Queue

Generated: `2026-04-22T08:02:14.246078+00:00`

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
| pending | 3598 |
| ported | 66 |
| superseded | 923 |

## First 100 Pending Commits

| SHA | Ticket | Subject |
| --- | ---: | --- |
| `c6df39955ccf` | #24 | fix: limit concurrent Modal sandbox creations to avoid deadlocks |
| `86eed141afdc` | #20 | fix: rebuild compressed payload before retry |
| `7e36468511c8` | #26 | fix: /clear command broken inside TUI (patch_stdout interference) |
| `9ee4fe41fe42` | #26 | Fix image_generate 'Event loop is closed' in gateway |
| `313d522b6162` | #21 | feat: add Polymarket prediction market skill (read-only) |
| `4447e7d71afa` | #20 | fix: add Kimi Code API support (api.kimi.com/coding/v1) |
| `3830bbda41e2` | #26 | fix: include url in web_extract trimmed results & fix docs |
| `fcde9be10d56` | #20 | fix: keep tool-call output runs intact during compression |
| `c7b6f423c713` | #20 | feat: auto-compress pathologically large gateway sessions (#628) |
| `bf048c8aecf0` | #21 | feat: add qmd optional skill — local knowledge base search |
| `8d719b180aea` | #20 | feat: git worktree isolation for parallel CLI sessions (--worktree / -w) |
| `4be783446af8` | #22 | fix: wire worktree flag into hermes CLI entry point + docs + tests |
| `5684c681216e` | #23 | Add logger.info/error for image extraction and delivery debugging |
| `542faf225fcc` | #23 | Fix Telegram image delivery for large (>5MB) images |
| `a68036756853` | #26 | fix tmux menus |
| `b8c3bc78417c` | #23 | feat: browser screenshot sharing via MEDIA: on all messaging platforms |
| `19b6f81ee78b` | #20 | fix: allow Anthropic API URLs as custom OpenAI-compatible endpoints |
| `f2105102763d` | #21 | feat: add prerequisites field to skill spec — hide skills with unmet dependencies |
| `d507f593d08b` | #26 | fix: respect config.yaml cwd in gateway, add sandbox_dir config option |
| `daa1f542f9ab` | #24 | fix: enhance shell detection in local environment configuration |
| `b10ff835663e` | #24 | fix: enhance PATH handling in local environment |
| `b383cafc440b` | #24 | refactor: rename and enhance shell detection in local environment |
| `bfa27d0a68de` | #20 | fix(cli): unify slash command autocomplete registry |
| `0df7df52f397` | #20 | test: expand slash command autocomplete coverage (PR #645 follow-up) |
| `b8120df860bb` | #21 | Revert "feat: skill prerequisites — hide skills with unmet runtime dependencies" |
| `d518f40e8bf1` | #26 | fix: improve browser command environment setup |
| `932d59646683` | #25 | feat: enhance systemd unit and install script for browser dependencies |
| `9d3a44e0e870` | #20 | fix: validate /model values before saving |
| `90fa9e54ca0a` | #20 | fix: guard validate_requested_model + expand test coverage (PR #649 follow-up) |
| `7b1f40dd009d` | #26 | Improve error handling and logging in code execution tool |
| `77f47768dde5` | #20 | fix: improve /history message display |
| `245d1743592b` | #20 | feat: validate /model against live API instead of hardcoded lists |
| `8c734f2f2767` | #20 | fix: remove OpenRouter '/' format enforcement — let API probe be the authority |
| `4a09ae298573` | #20 | chore: remove dead module stubs from test_cli_init.py |
| `66d3e6a0c2c3` | #20 | feat: provider switching via /model + enhanced model display |
| `132e5ec179f5` | #26 | fix: resolve 'auto' provider in /model display + update gateway handler |
| `f824c104298e` | #26 | feat: enhance config migration with new environment variable tracking |
| `7ad6fc8a408c` | #26 | fix: gateway /model also needs normalize_provider for 'auto' resolution |
| `34792dd907df` | #26 | fix: resolve 'auto' provider properly via credential detection |
| `666f2dd4868a` | #20 | feat: /provider command + fix gateway bugs + harden parse_model_input |
| `d07d867718a1` | #20 | Fix empty tool selection persistence |
| `a23bcb81ceb5` | #22 | fix: improve /model user feedback + update docs |
| `cf810c2950fd` | #20 | fix: pre-process CLI clipboard images through vision tool instead of raw embedding |
| `333e4abe3032` | #20 | fix: Initialize Skills Hub on list |
| `9eee529a7fec` | #20 | fix: detect and warn on file re-read loops after context compression |
| `e28dc13cd5d3` | #26 | fix: store and close log file handles in rl_training_tool |
| `7891050e06b5` | #26 | fix: use Path.read_text() instead of open() in browser_tool |
| `e2fe1373f31f` | #20 | fix: escalate read/search blocking, track search loops, filter completed todos |
| `081079da629c` | #26 | fix(setup): correct import of get_codex_model_ids in setup wizard |
| `67421ed74f2e` | #20 | fix: update test_non_empty_has_markers to match todo filtering behavior |
| `ceefe367562f` | #26 | docs: clarify Telegram token regex constraint |
| `d0f84c096406` | #20 | fix: log exceptions instead of silently swallowing in cron scheduler |
| `0c3253a4859c` | #20 | fix: mock asyncio.run in mirror test to prevent event loop destruction |
| `4d53b7ccaa0d` | #26 | Add OpenRouter app attribution headers to skills_guard and trajectory_compressor |
| `60b6abefd98f` | #20 | feat: session naming with unique titles, auto-lineage, rich listing, resume by name |
| `4fdd6c0dac1a` | #20 | fix: harden session title system + add /title to gateway |
| `34b4fe495e7b` | #20 | fix: add title validation — sanitize, length limit, control char stripping |
| `2b8856865339` | #22 | docs: add session naming documentation across all doc files |
| `7791174cedd5` | #20 | feat: add --fuck-it-ship-it flag to bypass dangerous command approvals |
| `3fb8938cd35c` | #20 | fix: search_files now reports error for non-existent paths instead of silent empty results |
| `95b1130485a2` | #20 | fix: normalize incompatible models when provider resolves to Codex |
| `26bb56b77546` | #20 | feat: add /resume command to gateway for switching to named sessions |
| `a5461e07bf4c` | #23 | feat: register title, resume, and other missing commands with platform menus |
| `a7f9721785af` | #23 | feat: register remaining commands with platform menus |
| `1f1caa836abe` | #26 | fix: error out when hermes -w is used outside a git repo |
| `c0520223fda4` | #20 | fix: clipboard BMP conversion file loss and broken test |
| `97b1c76b1430` | #20 | test: add regression test for #712 (setup wizard codex import) |
| `20c6573e0aa4` | #26 | docs: comprehensive AGENTS.md audit and corrections |
| `ecac6321c420` | #20 | feat: interactive session browser with search filtering (#718) |
| `4f0402ed3a51` | #20 | chore: remove all NOUS_API_KEY references |
| `3aded1d4e5e9` | #20 | feat: display previous messages when resuming a session in CLI |
| `491605cfea39` | #20 | feat: add high-value tool result hints for patch and search_files (#722) |
| `cf63b2471f8e` | #22 | docs: add resume history display to sessions, CLI, config, and AGENTS docs |
| `d9f373654b4a` | #20 | feat: enhance auxiliary model configuration and environment variable handling |
| `5ae0b731d011` | #20 | fix: harden auxiliary model config — gateway bridge, vision safety, tests |
| `192501528f87` | #26 | docs: add Auxiliary Model Configuration section to AGENTS.md |
| `7c30ac21412c` | #21 | fix: overhaul ascii-art skill with working sources (#662) |
| `ae4a674c8430` | #20 | feat: add 'openai' as auxiliary provider option |
| `f996d7950b7a` | #20 | fix: trust user-selected models with OpenAI Codex provider |
| `71e81728ac5c` | #20 | feat: Codex OAuth vision support + multimodal content adapter |
| `2d1a1c1c4755` | #20 | refactor: remove redundant 'openai' auxiliary provider, clean up docs |
| `99f758217538` | #21 | chore: move Solana skill to optional-skills/ |
| `2394e18729b0` | #26 | fix: add context to interruption messages for model awareness |
| `7185a66b9662` | #21 | feat: enhance Solana skill with USD pricing, token names, smart wallet output |
| `2036c22f8846` | #26 | fix: macOS browser/code-exec socket path exceeds Unix limit (#374) |
| `37752ff1ac5e` | #22 | feat: bell_on_complete — terminal bell when agent finishes |
| `4d7d9d971556` | #26 | fix: add diagnostic logging to browser tool for errors.log |
| `763c6d104d02` | #20 | fix: unify gateway session hygiene with agent compression config |
| `24f549a6929a` | #23 | feat: add Signal messenger gateway platform (#405) |
| `161436cfdd94` | #20 | feat: simple fallback model for provider resilience |
| `0c4cff352a05` | #22 | docs: add Signal messenger documentation across all doc surfaces |
| `4cfb66bac263` | #26 | docs: list all supported fallback providers with env var names |
| `b3765c28d0ac` | #20 | fix: restrict fallback providers to actual hermes providers |
| `b7d6eae64c16` | #22 | fix: Signal adapter parity pass — integration gaps, clawdbot features, env var simplification |
| `7241e8784a0e` | #20 | feat: hermes skills — enable/disable individual skills and categories (#642) |
| `fcd899f88819` | #23 | docs: add platform integration checklist for new gateway adapters |
| `3b312d45c5f6` | #26 | fix: show fallback_model as commented-out YAML example in config |
| `a8bf414f4a86` | #21 | feat: browser console/errors tool, annotated screenshots, auto-recording, and dogfood QA skill |
| `3ffaac00dd05` | #22 | feat: bell_on_complete — terminal bell when agent finishes |
| `67275641f848` | #20 | fix: unify gateway session hygiene with agent compression config |

